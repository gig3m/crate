//! Queue modes: shuffle and repeat, plus keeping the engine's upcoming
//! tracks in step with `Now::tracks` (gapless appends and reorders).

use gpui::Context;

use crate::{Crate, Source, plex::Track};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum Repeat {
    #[default]
    Off,
    All,
    One,
}

impl Crate {
    /// Engine sources for `tracks`.
    pub(crate) fn sources(&self, tracks: &[Track]) -> Vec<Source> {
        let (base, token) = self.plex.clone();
        tracks
            .iter()
            .map(|t| {
                let id = t.part_key.split('/').nth(3).unwrap_or("x").to_string();
                let ext = t.part_key.rsplit('.').next().unwrap_or("bin").to_string();
                Source::Http {
                    url: format!("{base}{}", t.part_key),
                    token: token.clone(),
                    cache: std::path::PathBuf::from(format!("/scratch/crate-cache/part-{id}.{ext}")),
                }
            })
            .collect()
    }

    /// Tracks the engine should get starting at queue index `from`: just that
    /// one under repeat-one (it ends, and we replay it), else the rest.
    pub(crate) fn engine_slice(&self, tracks: &[Track], from: usize) -> std::ops::Range<usize> {
        if self.repeat == Repeat::One { from..(from + 1).min(tracks.len()) } else { from..tracks.len() }
    }

    /// Hand the engine the tracks it hasn't opened yet, in `now.tracks` order.
    /// Retries if the decoder opens a track meanwhile.
    pub(crate) fn sync_upcoming(&mut self) {
        let (Some(engine), Some(now)) = (self.engine.as_ref(), self.now.as_ref()) else { return };
        for _ in 0..5 {
            let started = engine.started();
            let first = now.start_idx + started;
            if first > now.tracks.len() {
                return;
            }
            let range = if self.repeat == Repeat::One { first..first } else { first..now.tracks.len() };
            let srcs = self.sources(&now.tracks[range.clone()]);
            if engine.replace_upcoming(started, srcs) {
                let end = range.end;
                if let Some(n) = self.now.as_mut() {
                    n.engine_end = end;
                }
                return;
            }
        }
    }

    pub(crate) fn toggle_shuffle(&mut self, cx: &mut Context<Self>) {
        self.shuffle = !self.shuffle;
        let started = self.engine.as_ref().map(|e| e.started()).unwrap_or(0);
        if let Some(n) = self.now.as_mut() {
            // Only reorder what the decoder hasn't opened (and never the current track).
            let from = (n.start_idx + started).max(n.idx + 1).min(n.tracks.len());
            if self.shuffle {
                crate::shuffle(&mut n.tracks[from..]);
            } else {
                let pos: std::collections::HashMap<&str, usize> =
                    n.orig_order.iter().enumerate().map(|(i, k)| (k.as_str(), i)).collect();
                n.tracks[from..].sort_by_key(|t| pos.get(t.rating_key.as_str()).copied().unwrap_or(usize::MAX));
            }
        }
        self.sync_upcoming();
        let _ = self.mpris.send(crate::mpris::Update::Modes { shuffle: self.shuffle, repeat: self.repeat });
        cx.notify();
    }

    pub(crate) fn cycle_repeat(&mut self, cx: &mut Context<Self>) {
        self.repeat = match self.repeat {
            Repeat::Off => Repeat::All,
            Repeat::All => Repeat::One,
            Repeat::One => Repeat::Off,
        };
        self.sync_upcoming();
        let _ = self.mpris.send(crate::mpris::Update::Modes { shuffle: self.shuffle, repeat: self.repeat });
        cx.notify();
    }

    pub(crate) fn set_modes(&mut self, shuffle: Option<bool>, repeat: Option<Repeat>, cx: &mut Context<Self>) {
        if let Some(r) = repeat {
            self.repeat = r;
            self.sync_upcoming();
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
