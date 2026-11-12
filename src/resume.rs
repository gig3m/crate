//! Resume where you left off: the queue, the track and the position are
//! saved when a track starts, on pause and on quit, and come back paused at
//! the next launch (Play picks up from the saved position).

use gpui::Context;
use serde::{Deserialize, Serialize};

use crate::{Crate, plex::Track};

#[derive(Serialize, Deserialize)]
struct Saved {
    context: String,
    context_key: Option<String>,
    tracks: Vec<Track>,
    idx: usize,
    elapsed_ms: u64,
    /// The station a radio queue refills from.
    refill: Option<String>,
    orig_order: Vec<String>,
    #[serde(default)]
    shuffle: bool,
}

/// Off for test runs that set CRATE_NO_RESUME (they mustn't read or
/// overwrite the real resume point).
fn enabled() -> bool {
    std::env::var_os("CRATE_NO_RESUME").is_none()
}

fn path() -> std::path::PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config"));
    base.join("crate/resume.json")
}

impl Crate {
    pub(crate) fn save_resume(&self) {
        if !enabled() {
            return;
        }
        let Some(n) = self.now.as_ref() else { return };
        if n.tracks.is_empty() {
            return;
        }
        let saved = Saved {
            context: n.context.clone(),
            context_key: n.context_key.clone(),
            tracks: n.tracks.clone(),
            idx: n.idx.min(n.tracks.len() - 1),
            elapsed_ms: if n.finished { 0 } else { n.elapsed_ms },
            refill: n.refill.clone(),
            orig_order: n.orig_order.clone(),
            shuffle: self.shuffle,
        };
        let p = path();
        let res = (|| -> std::io::Result<()> {
            std::fs::create_dir_all(p.parent().unwrap_or(std::path::Path::new(".")))?;
            let tmp = p.with_extension(format!("tmp{}", std::process::id()));
            std::fs::write(&tmp, serde_json::to_vec(&saved)?)?;
            std::fs::rename(tmp, &p)
        })();
        if let Err(e) = res {
            eprintln!("[resume] couldn't save: {e}");
        }
    }

    /// At launch: the last queue, paused where it was left.
    pub(crate) fn restore_resume(&mut self, cx: &mut Context<Self>) {
        if self.now.is_some() || !enabled() {
            return;
        }
        let Ok(bytes) = std::fs::read(path()) else { return };
        let Ok(s) = serde_json::from_slice::<Saved>(&bytes) else { return };
        if s.tracks.is_empty() {
            return;
        }
        let idx = s.idx.min(s.tracks.len() - 1);
        // Left in the last seconds of a track: start it from the top rather
        // than seeking to (or past) its end.
        let len = s.tracks[idx].duration_ms;
        let elapsed = if len > 0 && s.elapsed_ms + 5000 >= len { 0 } else { s.elapsed_ms };
        self.shuffle = s.shuffle;
        let _ = self.mpris.send(crate::mpris::Update::Modes { shuffle: self.shuffle, repeat: self.repeat });
        for t in &s.tracks {
            self.request_thumb(&t.thumb.clone());
        }
        self.queue_seq += 1;
        let tracks = s.tracks.clone();
        // A mix's queue: give back its page (the queue as its batch), so
        // "Playing from" still opens it.
        if let Some(id) = s.context_key.as_deref().and_then(|k| k.strip_prefix("mix:"))
            && let Some(source) = crate::mix::MixSource::from_id(id)
        {
            self.mixes.entry(id.to_string()).or_insert_with(|| crate::mix::MixPage {
                title: s.context.clone(),
                desc: String::new(),
                source,
                station: s.refill.clone(),
                tracks: Some(tracks.clone()),
                error: None,
                load_gen: 0,
            });
        }
        self.now = Some(crate::Now::restored(s.context, s.context_key, s.tracks, idx, elapsed, s.refill, s.orig_order, self.queue_seq));
        // Loudness for leveling, fetched in the background like any queue's.
        if self.leveling != crate::theme::Leveling::Off {
            self.fill_gains(&tracks, idx, cx);
        }
        cx.notify();
    }
}
