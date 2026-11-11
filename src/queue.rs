//! Queue modes: shuffle and repeat, plus keeping the engine's upcoming
//! tracks in step with `Now::tracks` (gapless appends and reorders).

use gpui::Context;

use crate::{Crate, Item, Source, plex::Track};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum Repeat {
    #[default]
    Off,
    All,
    One,
}

impl Crate {
    /// Linear leveling gain for `t` under the current setting: 10^(dB/20),
    /// held under 1/peak so it can't clip; 1.0 when off or not analyzed.
    pub(crate) fn level_gain(&self, t: &Track) -> f32 {
        use crate::theme::Leveling;
        let Some(g) = self.gains.get(&t.rating_key) else { return 1.0 };
        let album_in_order = !self.shuffle && self.now.as_ref().is_some_and(|n| n.context_key.as_deref() == Some(t.album_key.as_str()));
        let use_album = match self.leveling {
            Leveling::Off => return 1.0,
            Leveling::Track => false,
            Leveling::Album => true,
            Leveling::Smart => album_in_order,
        };
        linear_gain(g, use_album)
    }

    /// Engine items for `tracks`, with leveling gains and sweet fades.
    /// `prev` is the track played just before `tracks[0]` (None when the
    /// first item starts the engine).
    pub(crate) fn sources(&self, tracks: &[Track], prev: Option<&Track>) -> Vec<Item> {
        let fade_ms = self.fade_secs.min(12) * 1000;
        tracks
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let before = if i == 0 { prev } else { tracks.get(i - 1) };
                // Sweet fades only between albums: gapless inside an album is never broken.
                let fade = match before {
                    Some(b) if fade_ms > 0 && b.album_key != t.album_key => fade_ms,
                    _ => 0,
                };
                Item { src: self.source(t), gain: self.level_gain(t), fade_ms: fade }
            })
            .collect()
    }

    fn source(&self, t: &Track) -> Source {
        // Kept offline (on this server): play the local copy.
        if let Some(kept) = self.offline.local(&t.part_key) {
            return Source::File(kept);
        }
        let (base, token) = self.plex.clone();
        let id = t.part_key.split('/').nth(3).unwrap_or("x").to_string();
        let ext = t.part_key.rsplit('.').next().unwrap_or("bin").to_string();
        Source::Http {
            url: format!("{base}{}", t.part_key),
            token,
            cache: std::path::PathBuf::from(format!("/scratch/crate-cache/part-{id}.{ext}")),
        }
    }

    /// Tracks the engine should get starting at queue index `from`: just that
    /// one under repeat-one (it ends, and we replay it), else the rest.
    pub(crate) fn engine_slice(&self, tracks: &[Track], from: usize) -> std::ops::Range<usize> {
        if self.repeat == Repeat::One { from..(from + 1).min(tracks.len()) } else { from..tracks.len() }
    }

    /// Hand the engine the tracks it hasn't opened yet, in `now.tracks` order.
    /// Retries if the decoder opens a track meanwhile.
    pub(crate) fn sync_upcoming(&mut self) {
        for _ in 0..5 {
            let Some(started) = self.engine.as_ref().map(|e| e.started()) else { return };
            if self.hand_over(started) {
                return;
            }
        }
    }

    /// Replace the engine's unopened tracks with `now.tracks` from the point
    /// `started` implies. False if the decoder has moved on (or closed).
    fn hand_over(&mut self, started: usize) -> bool {
        let (Some(engine), Some(now)) = (self.engine.as_ref(), self.now.as_ref()) else { return true };
        let first = now.start_idx + started;
        if first > now.tracks.len() {
            return true;
        }
        // Repeat-one hands the engine just the current track; if the decoder
        // hasn't opened it yet (started == 0), it's still upcoming.
        let range = match self.repeat {
            Repeat::One if started == 0 => first..(first + 1).min(now.tracks.len()),
            Repeat::One => first..first,
            _ => first..now.tracks.len(),
        };
        let prev = range.start.checked_sub(1).and_then(|p| now.tracks.get(p));
        let srcs = self.sources(&now.tracks[range.clone()], prev);
        if !engine.replace_upcoming(started, srcs) {
            return false;
        }
        let end = range.end;
        if let Some(n) = self.now.as_mut() {
            n.engine_end = end;
        }
        true
    }

    /// Play next (right after what the engine has already opened) or add to
    /// the end of the queue. Nothing playing: these start a queue.
    pub(crate) fn enqueue(&mut self, tracks: Vec<Track>, next: bool, label: String, cx: &mut Context<Self>) {
        if tracks.is_empty() {
            self.status = format!("{label}: nothing to queue").into();
            cx.notify();
            return;
        }
        let n = tracks.len();
        if std::env::var_os("CRATE_TRACE").is_some() {
            let st = self.now.as_ref().map(|n| (n.idx, n.tracks.len(), n.finished, n.stopped, n.engine_end));
            eprintln!("[crate] enqueue {n} next={next} now={st:?} engine={} closed={}", self.engine.is_some(), self.engine.as_ref().is_some_and(|e| e.is_closed()));
        }
        for t in &tracks {
            self.request_thumb(&t.thumb.clone());
        }
        if self.now.is_none() {
            self.start_queue(label, None, tracks, 0, cx);
            return;
        }
        // Insert and hand over against the SAME `started`; if the decoder opens a
        // track in between, the engine refuses it and we redo it from the new point.
        let mut placed = None;
        for _ in 0..5 {
            // A closed engine (its queue ran out) takes no more tracks: treat it
            // as none; the poll loop or the resume below plays what's added.
            let started = self.engine.as_ref().filter(|e| !e.is_closed()).map(|e| e.started());
            // What the decoder got through, even if it's closed: never insert before that.
            let decoded = self.engine.as_ref().map(|e| e.started()).unwrap_or(0);
            let Some(now) = self.now.as_mut() else { return };
            let opened = now.start_idx + decoded;
            let at = if next { (now.idx + 1).max(opened) } else { now.tracks.len() }.min(now.tracks.len());
            now.tracks.splice(at..at, tracks.iter().cloned());
            // Shuffle-off order: right after the current track, or at the end.
            let cur_key = now.tracks.get(now.idx).map(|t| t.rating_key.clone());
            let opos = if next { cur_key.and_then(|k| now.orig_order.iter().position(|o| *o == k)).map(|p| p + 1).unwrap_or(now.orig_order.len()) } else { now.orig_order.len() };
            now.orig_order.splice(opos..opos, tracks.iter().map(|t| t.rating_key.clone()));
            // Tracks before `at` that were handed to the engine shift with the insert.
            if now.engine_end > at {
                now.engine_end += n;
            }
            match started {
                // No live engine (finished or stopped): nothing to hand over.
                None => {
                    placed = Some(at);
                    break;
                }
                Some(s) if self.hand_over(s) => {
                    placed = Some(at);
                    break;
                }
                Some(_) => {
                    // Undo and retry from where the decoder is now.
                    if let Some(now) = self.now.as_mut() {
                        now.tracks.drain(at..at + n);
                        now.orig_order.drain(opos..opos + n);
                        if now.engine_end > at + n {
                            now.engine_end -= n;
                        }
                    }
                }
            }
        }
        let Some(at) = placed else {
            self.status = "Couldn't queue that (playback moved on); try again".into();
            cx.notify();
            return;
        };
        self.status = format!("{} {label}", if next { "Playing next:" } else { "Added to queue:" }).into();
        // The queue already ran out (not stopped): carry on with the new tracks.
        if let Some(now) = self.now.as_ref()
            && now.finished
            && !now.stopped
        {
            let (all, qid) = (now.tracks.clone(), now.queue_id);
            let missing: Vec<String> = self.missing_gains(&all[at.min(all.len())..(at + 50).min(all.len())], 0);
            if missing.is_empty() {
                self.play_from(at, 0, cx);
            } else {
                // Leveling: the first new track needs its gain before it starts (bounded).
                let rx = crate::plex_call(move |c| async move {
                    tokio::time::timeout(std::time::Duration::from_millis(1500), c.track_gains(missing)).await.map_err(|_| anyhow::anyhow!("timed out"))?
                });
                cx.spawn(async move |this, cx| {
                    let res = rx.await;
                    this.update(cx, |this, cx| {
                        if let Ok(Ok(found)) = res {
                            this.gains.extend(found);
                        }
                        // Still the same finished queue, and nothing else started it.
                        if let Some(n) = this.now.as_ref()
                            && n.queue_id == qid
                            && n.finished
                            && !n.stopped
                        {
                            this.play_from(at, 0, cx);
                            let all = this.now.as_ref().map(|n| n.tracks.clone()).unwrap_or_default();
                            this.fill_gains(&all, at, cx);
                        }
                    })
                    .ok();
                })
                .detach();
            }
        } else if self.leveling != crate::theme::Leveling::Off {
            // Loudness for the new tracks, then retune the engine's upcoming ones.
            let all = self.now.as_ref().map(|n| n.tracks.clone()).unwrap_or_default();
            self.fill_gains(&all, at, cx);
        }
        cx.notify();
    }

    pub(crate) fn toggle_shuffle(&mut self, cx: &mut Context<Self>) {
        self.shuffle = !self.shuffle;
        // Reorder and hand over against the SAME `started`: if the decoder opens a
        // track in between, replace_upcoming fails and we redo it from the new point.
        for _ in 0..5 {
            let started = self.engine.as_ref().map(|e| e.started());
            let Some(n) = self.now.as_mut() else { break };
            let from = (n.start_idx + started.unwrap_or(0)).max(n.idx + 1).min(n.tracks.len());
            let before = n.tracks[from..].to_vec();
            if self.shuffle {
                crate::shuffle(&mut n.tracks[from..]);
            } else {
                let pos: std::collections::HashMap<&str, usize> =
                    n.orig_order.iter().enumerate().map(|(i, k)| (k.as_str(), i)).collect();
                n.tracks[from..].sort_by_key(|t| pos.get(t.rating_key.as_str()).copied().unwrap_or(usize::MAX));
            }
            let Some(started) = started else { break };
            if self.hand_over(started) {
                break;
            }
            // The decoder opened a track meanwhile: the engine refused this order,
            // so undo it and rebuild from the new position.
            if let Some(n) = self.now.as_mut() {
                n.tracks.truncate(from);
                n.tracks.extend(before);
            }
        }
        let _ = self.mpris.send(crate::mpris::Update::Modes { shuffle: self.shuffle, repeat: self.repeat });
        cx.notify();
    }

    pub(crate) fn cycle_repeat(&mut self, cx: &mut Context<Self>) {
        self.repeat = match self.repeat {
            Repeat::Off => Repeat::All,
            Repeat::All => Repeat::One,
            Repeat::One => Repeat::Off,
        };
        self.apply_repeat(cx);
        let _ = self.mpris.send(crate::mpris::Update::Modes { shuffle: self.shuffle, repeat: self.repeat });
        cx.notify();
    }

    pub(crate) fn set_modes(&mut self, shuffle: Option<bool>, repeat: Option<Repeat>, cx: &mut Context<Self>) {
        if let Some(r) = repeat {
            self.repeat = r;
            self.apply_repeat(cx);
        }
        if let Some(s) = shuffle
            && s != self.shuffle
        {
            self.toggle_shuffle(cx);
            return;
        }
        let _ = self.mpris.send(crate::mpris::Update::Modes { shuffle: self.shuffle, repeat: self.repeat });
        cx.notify();
    }

    /// Push the repeat mode to the engine. Repeat-one must loop the track
    /// that's playing: if the decoder has already opened the next one (the
    /// last seconds of a track), restart at the current position instead.
    fn apply_repeat(&mut self, cx: &mut Context<Self>) {
        if self.repeat == Repeat::One
            && let (Some(e), Some(n)) = (self.engine.as_ref(), self.now.as_ref())
            && n.start_idx + e.started() > n.idx + 1
        {
            let (i, ms) = (n.idx, n.elapsed_ms);
            let paused = n.paused;
            self.play_from(i, ms, cx);
            if paused {
                self.transport(crate::mpris::Transport::Pause, cx);
            }
            return;
        }
        self.sync_upcoming();
    }

    /// After the engine ran out: repeat-one replays, repeat-all wraps.
    /// Returns true if it started something.
    pub(crate) fn repeat_on_finish(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(n) = self.now.as_ref() else { return false };
        if !n.finished || n.stopped {
            return false;
        }
        match self.repeat {
            Repeat::One => {
                let i = n.idx;
                self.play_from(i, 0, cx);
                true
            }
            Repeat::All if n.engine_end >= n.tracks.len() && n.refill.is_none() => {
                self.play_from(0, 0, cx);
                true
            }
            _ => false,
        }
    }
}

/// 10^(dB/20) for the album or track gain (falling back to the other when
/// one is missing), held under 1/peak so it can't clip. dB is bounded to
/// -24..+12; exactly 1.0 when there's nothing to apply.
pub(crate) fn linear_gain(g: &crate::plex::Gain, use_album: bool) -> f32 {
    let (db, peak) = if use_album { (g.album_db.or(g.track_db), g.album_peak.or(g.track_peak)) } else { (g.track_db.or(g.album_db), g.track_peak.or(g.album_peak)) };
    let Some(db) = db else { return 1.0 };
    let mut lin = 10f32.powf(db.clamp(-24.0, 12.0) / 20.0);
    match peak.filter(|p| *p > 0.0) {
        Some(p) => lin = lin.min(1.0 / p),
        // Without a peak a boost could clip: attenuate only.
        None => lin = lin.min(1.0),
    }
    if (lin - 1.0).abs() < 1e-4 { 1.0 } else { lin }
}

#[cfg(test)]
mod gain_tests {
    use super::linear_gain;
    use crate::plex::Gain;

    #[test]
    fn gains() {
        let g = Gain { track_db: Some(-5.46), track_peak: Some(0.97), album_db: Some(-8.27), album_peak: Some(0.99) };
        assert!((linear_gain(&g, false) - 10f32.powf(-5.46 / 20.0)).abs() < 1e-5);
        assert!((linear_gain(&g, true) - 10f32.powf(-8.27 / 20.0)).abs() < 1e-5);
        // A boost is held under the peak: +6 dB on a 0.8 peak gives 1/0.8.
        let quiet = Gain { track_db: Some(6.0), track_peak: Some(0.8), ..Default::default() };
        assert!((linear_gain(&quiet, false) - 1.25).abs() < 1e-5);
        // Album mode falls back to the track's gain when the album's is missing.
        assert!((linear_gain(&quiet, true) - 1.25).abs() < 1e-5);
        // Nothing analyzed: unity, exactly.
        assert_eq!(linear_gain(&Gain::default(), false), 1.0);
        assert_eq!(linear_gain(&Gain { track_db: Some(0.0), ..Default::default() }, false), 1.0);
        // No peak known: never boost.
        assert_eq!(linear_gain(&Gain { track_db: Some(6.0), ..Default::default() }, false), 1.0);
    }
}
