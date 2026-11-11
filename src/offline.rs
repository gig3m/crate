//! Offline albums: "Keep offline" downloads an album's original files to
//! ~/.local/share/crate/offline (not the disposable /scratch cache); playback
//! uses them when present. index.json lists what's kept.

use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use gpui::Context;
use serde::{Deserialize, Serialize};

use crate::{Crate, plex::{Album, Track}};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeptTrack {
    pub rating_key: String,
    pub part_key: String,
    pub title: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeptAlbum {
    pub key: String,
    pub title: String,
    pub artist: String,
    pub thumb: String,
    pub tracks: Vec<KeptTrack>,
    /// Every track is on disk.
    pub complete: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Index {
    pub albums: Vec<KeptAlbum>,
}

impl Index {
    pub fn get(&self, key: &str) -> Option<&KeptAlbum> {
        self.albums.iter().find(|a| a.key == key)
    }
}

/// The album downloading now.
pub struct Job {
    pub key: String,
    pub done_tracks: usize,
    pub total_tracks: usize,
    pub bytes: Arc<AtomicU64>,
    pub cancel: Arc<AtomicBool>,
}

#[derive(Default)]
pub struct Offline {
    pub index: Index,
    pub job: Option<Job>,
    pub queue: VecDeque<String>,
}

pub fn dir() -> PathBuf {
    let base = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/share"));
    base.join("crate/offline")
}

/// Where a part's file lives once kept.
pub fn file_for(part_key: &str) -> PathBuf {
    let id = part_key.split('/').nth(3).unwrap_or("x");
    let ext = part_key.rsplit('.').next().filter(|e| e.len() <= 5 && !e.contains('/')).unwrap_or("bin");
    dir().join(format!("part-{id}.{ext}"))
}

pub fn load() -> Index {
    std::fs::read_to_string(dir().join("index.json")).ok().and_then(|r| serde_json::from_str(&r).ok()).unwrap_or_default()
}

fn save(idx: &Index) {
    let d = dir();
    let _ = std::fs::create_dir_all(&d);
    let tmp = d.join("index.json.tmp");
    if std::fs::write(&tmp, serde_json::to_vec_pretty(idx).unwrap_or_default()).is_ok() {
        let _ = std::fs::rename(&tmp, d.join("index.json"));
    }
}

/// Bytes on disk for a kept album.
pub fn album_bytes(a: &KeptAlbum) -> u64 {
    a.tracks.iter().filter_map(|t| std::fs::metadata(file_for(&t.part_key)).ok()).map(|m| m.len()).sum()
}

pub fn fmt_bytes(b: u64) -> String {
    match b {
        0..1_000_000 => format!("{} KB", b / 1000),
        1_000_000..1_000_000_000 => format!("{:.0} MB", b as f64 / 1e6),
        _ => format!("{:.1} GB", b as f64 / 1e9),
    }
}

impl Crate {
    /// Keep an album offline (or, if kept, stop keeping it).
    pub(crate) fn toggle_offline(&mut self, album: Album, tracks: Vec<Track>, cx: &mut Context<Self>) {
        if self.offline.index.get(&album.rating_key).is_some() {
            self.remove_offline(&album.rating_key, cx);
            return;
        }
        let kept = KeptAlbum {
            key: album.rating_key.clone(),
            title: album.title.clone(),
            artist: album.artist.clone(),
            thumb: album.thumb.clone(),
            tracks: tracks
                .iter()
                .filter(|t| !t.part_key.is_empty())
                .map(|t| KeptTrack { rating_key: t.rating_key.clone(), part_key: t.part_key.clone(), title: t.title.clone() })
                .collect(),
            complete: false,
        };
        self.offline.index.albums.push(kept);
        save(&self.offline.index);
        self.offline.queue.push_back(album.rating_key);
        self.pump_offline(cx);
        cx.notify();
    }

    pub(crate) fn remove_offline(&mut self, key: &str, cx: &mut Context<Self>) {
        if let Some(j) = self.offline.job.as_ref().filter(|j| j.key == key) {
            j.cancel.store(true, Ordering::Relaxed);
        }
        self.offline.queue.retain(|k| k != key);
        if let Some(pos) = self.offline.index.albums.iter().position(|a| a.key == key) {
            let a = self.offline.index.albums.remove(pos);
            // Keep files another kept album still uses (the same part in a compilation).
            for t in &a.tracks {
                let shared = self.offline.index.albums.iter().any(|o| o.tracks.iter().any(|x| x.part_key == t.part_key));
                if !shared {
                    let p = file_for(&t.part_key);
                    let _ = std::fs::remove_file(&p);
                    let _ = std::fs::remove_file(p.with_extension("part"));
                }
            }
            save(&self.offline.index);
        }
        cx.notify();
    }

    /// Start the next queued album if nothing is downloading. Also resumes
    /// albums left incomplete by a previous run (called at startup).
    pub(crate) fn pump_offline(&mut self, cx: &mut Context<Self>) {
        if self.offline.job.is_some() {
            return;
        }
        let Some(key) = self.offline.queue.pop_front() else { return };
        let Some(album) = self.offline.index.get(&key).cloned() else { return self.pump_offline(cx) };
        let job = Job {
            key: key.clone(),
            done_tracks: 0,
            total_tracks: album.tracks.len(),
            bytes: Arc::new(AtomicU64::new(0)),
            cancel: Arc::new(AtomicBool::new(false)),
        };
        let (bytes, cancel) = (job.bytes.clone(), job.cancel.clone());
        self.offline.job = Some(job);
        cx.spawn(async move |this, cx| {
            let mut failed = None;
            for (i, t) in album.tracks.iter().enumerate() {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                let dest = file_for(&t.part_key);
                if !dest.exists() {
                    let (pk, b, c, d) = (t.part_key.clone(), bytes.clone(), cancel.clone(), dest.clone());
                    let mut rx = crate::plex_call(move |cl| async move { cl.download(&pk, &d, b, c).await });
                    // Refresh the progress display while the file comes in (GPUI
                    // timer: this task doesn't run on tokio).
                    let res = loop {
                        cx.background_executor().timer(Duration::from_millis(500)).await;
                        match rx.try_recv() {
                            Ok(r) => break Ok(r),
                            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                                if this.update(cx, |_, cx| cx.notify()).is_err() {
                                    return;
                                }
                            }
                            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => break Err(()),
                        }
                    };
                    match res {
                        Ok(Ok(_)) => {}
                        Ok(Err(e)) => {
                            eprintln!("[crate] offline: {}: {e:#}", t.title);
                            failed = Some(format!("{}: {e:#}", t.title));
                            break;
                        }
                        Err(()) => {
                            eprintln!("[crate] offline: download task ended without a result");
                            break;
                        }
                    }
                }
                if this
                    .update(cx, |this, cx| {
                        if let Some(j) = this.offline.job.as_mut() {
                            j.done_tracks = i + 1;
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }
            this.update(cx, |this, cx| {
                let cancelled = cancel.load(Ordering::Relaxed);
                this.offline.job = None;
                if let Some(a) = this.offline.index.albums.iter_mut().find(|a| a.key == key) {
                    a.complete = a.tracks.iter().all(|t| file_for(&t.part_key).exists());
                }
                save(&this.offline.index);
                if let Some(e) = failed.filter(|_| !cancelled) {
                    this.status = format!("Offline download stopped: {e}").into();
                }
                this.pump_offline(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}
