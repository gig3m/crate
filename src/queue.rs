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

    /// Engine items for `tracks`, with leveling gains.
    pub(crate) fn sources(&self, tracks: &[Track]) -> Vec<Item> {
        tracks.iter().map(|t| Item { src: self.source(t), gain: self.level_gain(t) }).collect()
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
        let range = if self.repeat == Repeat::One { first..first } else { first..now.tracks.len() };
        let srcs = self.sources(&now.tracks[range.clone()]);
        if !engine.replace_upcoming(started, srcs) {
            return false;
        }
        let end = range.end;
        if let Some(n) = self.now.as_mut() {
            n.engine_end = end;
        }
        true
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
