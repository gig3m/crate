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
}

fn path() -> std::path::PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config"));
    base.join("crate/resume.json")
}

impl Crate {
    pub(crate) fn save_resume(&self) {
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
        if self.now.is_some() {
            return;
        }
        let Ok(bytes) = std::fs::read(path()) else { return };
        let Ok(s) = serde_json::from_slice::<Saved>(&bytes) else { return };
        if s.tracks.is_empty() {
            return;
        }
        let idx = s.idx.min(s.tracks.len() - 1);
        for t in &s.tracks {
            self.request_thumb(&t.thumb.clone());
        }
        self.queue_seq += 1;
        self.now = Some(crate::Now::restored(s.context, s.context_key, s.tracks, idx, s.elapsed_ms, s.refill, s.orig_order, self.queue_seq));
        cx.notify();
    }
}
