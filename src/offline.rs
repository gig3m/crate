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
    /// Enough to play the album with the server unreachable (older index
    /// entries lack these until the album is next loaded from the server).
    #[serde(default)]
    pub artist: String,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub track_no: u32,
    #[serde(default)]
    pub disc: u32,
    /// The file this track had before the server replaced it: still played
    /// offline until the new one is kept, then deleted.
    #[serde(default)]
    pub prev_part: Option<String>,
}

impl KeptTrack {
    fn from_track(t: &Track) -> Self {
        KeptTrack {
            rating_key: t.rating_key.clone(),
            part_key: t.part_key.clone(),
            title: t.title.clone(),
            artist: t.artist.clone(),
            duration_ms: t.duration_ms,
            track_no: t.track_no,
            disc: t.disc,
            prev_part: None,
        }
    }
}

impl KeptAlbum {
    /// The album's tracks from the index, for playing without the server.
    pub fn to_tracks(&self) -> Vec<Track> {
        self.tracks
            .iter()
            .map(|k| Track {
                rating_key: k.rating_key.clone(),
                title: k.title.clone(),
                artist: if k.artist.is_empty() { self.artist.clone() } else { k.artist.clone() },
                album_artist: self.artist.clone(),
                duration_ms: k.duration_ms,
                part_key: k.part_key.clone(),
                album_key: self.key.clone(),
                album_title: self.title.clone(),
                thumb: self.thumb.clone(),
                track_no: k.track_no,
                disc: k.disc,
                playlist_item: 0,
            })
            .collect()
    }
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
    /// Bytes on disk, updated when a download run ends (not stat'd per frame).
    #[serde(default)]
    pub bytes: u64,
    /// Why the last download stopped, until it's retried.
    #[serde(default)]
    pub failed: Option<String>,
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
    /// Part keys of kept tracks on this server: the only ones played locally.
    pub kept_parts: std::collections::HashSet<String>,
}

impl Offline {
    pub fn new() -> Self {
        let index = load();
        let queue = index.albums.iter().filter(|a| !a.complete && a.failed.is_none()).map(|a| a.key.clone()).collect();
        let mut o = Offline { index, job: None, queue, kept_parts: Default::default() };
        o.reindex();
        o
    }

    fn reindex(&mut self) {
        self.kept_parts = self.index.albums.iter().flat_map(|a| a.tracks.iter().flat_map(|t| std::iter::once(t.part_key.clone()).chain(t.prev_part.clone()))).collect();
    }

    /// The local copy of a kept track, if it's on disk.
    pub fn local(&self, part_key: &str) -> Option<PathBuf> {
        if !self.kept_parts.contains(part_key) {
            return None;
        }
        file_for(part_key).filter(|p| p.exists())
    }
}

/// Offline files belong to one server: part ids are only unique per server.
static SCOPE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Set once at startup from the account (server machine id, else its URL).
pub fn set_scope(server_id: &str, url: &str) {
    // FNV-1a: a fixed algorithm, so the folder name never changes with the toolchain.
    let url_key = format!("url-{:016x}", url.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3)));
    let id: String = server_id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
    let key = if id.is_empty() { url_key.clone() } else { id };
    let _ = SCOPE.set(key.clone());
    // Albums kept before the server id was known move to the id's folder.
    if key != url_key {
        let root = dir().parent().map(|p| p.to_path_buf());
        if let Some(root) = root {
            let (old, new) = (root.join(&url_key), root.join(&key));
            if old.is_dir() {
                if new.exists() {
                    eprintln!("[crate] offline: both {} and {} exist; leaving {} alone", old.display(), new.display(), old.display());
                } else if let Err(e) = std::fs::rename(&old, &new) {
                    eprintln!("[crate] offline: couldn't move {} to {}: {e}", old.display(), new.display());
                }
            }
        }
    }
}

pub fn dir() -> PathBuf {
    let base = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/share"));
    base.join("crate/offline").join(SCOPE.get().map(String::as_str).unwrap_or("default"))
}

/// Where a part's file lives once kept: `/library/parts/<numeric id>/...`;
/// anything else isn't kept (two tracks must never share a file).
pub fn file_for(part_key: &str) -> Option<PathBuf> {
    let mut seg = part_key.split('/');
    let (_, lib, parts, id) = (seg.next(), seg.next(), seg.next(), seg.next());
    if lib != Some("library") || parts != Some("parts") {
        return None;
    }
    let id = id.filter(|i| !i.is_empty() && i.bytes().all(|b| b.is_ascii_digit()))?;
    let ext = part_key.rsplit('.').next().filter(|e| (1..=5).contains(&e.len()) && e.bytes().all(|b| b.is_ascii_alphanumeric())).unwrap_or("bin");
    Some(dir().join(format!("part-{id}.{ext}")))
}

/// Set when the index couldn't be read this session: saving would overwrite it.
static READ_FAILED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// A corrupt index is set aside (not overwritten) so its files stay findable.
pub fn load() -> Index {
    let p = dir().join("index.json");
    // Only a missing file means "nothing kept"; anything unreadable is set aside.
    let raw = match std::fs::read(&p) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Index::default(),
        // A read error isn't proof the file is bad: leave it, and don't save over it.
        Err(e) => {
            eprintln!("[crate] offline index can't be read ({e}); not saving over it this session");
            READ_FAILED.store(true, Ordering::Relaxed);
            return Index::default();
        }
    };
    match serde_json::from_slice(&raw) {
        Ok(i) => i,
        Err(e) => {
            let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            let aside = p.with_extension(format!("json.corrupt-{stamp}"));
            eprintln!("[crate] offline index unreadable ({e}); moved to {}", aside.display());
            let _ = std::fs::rename(&p, aside);
            Index::default()
        }
    }
}

fn save(idx: &Index) -> std::io::Result<()> {
    if READ_FAILED.load(Ordering::Relaxed) {
        return Err(std::io::Error::other("the offline index couldn't be read at startup; not overwriting it"));
    }
    let d = dir();
    std::fs::create_dir_all(&d)?;
    let tmp = d.join("index.json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(idx).unwrap_or_default())?;
    std::fs::rename(&tmp, d.join("index.json"))
}

/// Bytes on disk for a kept album (stats every file: call off the render path).
pub fn album_bytes(a: &KeptAlbum) -> u64 {
    a.tracks.iter().filter_map(|t| file_for(&t.part_key)).filter_map(|p| std::fs::metadata(p).ok()).map(|m| m.len()).sum()
}

pub fn fmt_bytes(b: u64) -> String {
    match b {
        0..1_000_000 => format!("{} KB", b / 1000),
        1_000_000..1_000_000_000 => format!("{:.0} MB", b as f64 / 1e6),
        _ => format!("{:.1} GB", b as f64 / 1e9),
    }
}

impl Crate {
    fn save_offline(&mut self) {
        self.offline.reindex();
        if let Err(e) = save(&self.offline.index) {
            self.status = format!("Couldn't save the offline list: {e}").into();
        }
    }

    /// Keep an album offline (or, if kept, stop keeping it).
    pub(crate) fn toggle_offline(&mut self, album: Album, tracks: Vec<Track>, cx: &mut Context<Self>) {
        if self.offline.index.get(&album.rating_key).is_some() {
            self.remove_offline(&album.rating_key, cx);
        } else {
            self.keep_offline(album, tracks, cx);
        }
    }

    /// Retry a failed album.
    pub(crate) fn retry_offline(&mut self, key: &str, cx: &mut Context<Self>) {
        let busy = self.offline.queue.iter().any(|k| k == key) || self.offline.job.as_ref().is_some_and(|j| j.key == key);
        if busy {
            return;
        }
        if let Some(a) = self.offline.index.albums.iter_mut().find(|a| a.key == key) {
            a.failed = None;
            self.offline.queue.push_back(key.to_string());
            self.save_offline();
            self.pump_offline(cx);
            cx.notify();
        }
    }

    pub(crate) fn keep_offline(&mut self, album: Album, tracks: Vec<Track>, cx: &mut Context<Self>) {
        if self.offline.index.get(&album.rating_key).is_some() {
            return;
        }
        let kept = KeptAlbum {
            key: album.rating_key.clone(),
            title: album.title.clone(),
            artist: album.artist.clone(),
            thumb: album.thumb.clone(),
            tracks: tracks
                .iter()
                .filter(|t| file_for(&t.part_key).is_some())
                .map(KeptTrack::from_track)
                .collect(),
            complete: false,
            bytes: 0,
            failed: None,
        };
        if kept.tracks.is_empty() {
            self.status = format!("{} has no files Crate can keep offline", album.title).into();
            cx.notify();
            return;
        }
        self.offline.index.albums.push(kept);
        self.save_offline();
        self.offline.queue.push_back(album.rating_key);
        self.pump_offline(cx);
        cx.notify();
    }

    /// A kept album's tracks that are on disk, for playing without the
    /// server (None when nothing is).
    pub(crate) fn offline_tracks(&self, album_key: &str) -> Option<Vec<Track>> {
        let kept = self.offline.index.get(album_key)?;
        // A track whose replacement isn't kept yet plays its previous file.
        let on_disk: Vec<Track> = kept
            .to_tracks()
            .into_iter()
            .zip(&kept.tracks)
            .filter_map(|(mut t, k)| {
                if self.offline.local(&t.part_key).is_none() {
                    t.part_key = k.prev_part.clone().filter(|p| self.offline.local(p).is_some())?;
                }
                Some(t)
            })
            .collect();
        (!on_disk.is_empty()).then_some(on_disk)
    }

    /// Fill in kept tracks' details from a fresh server listing (albums kept
    /// before the index carried them). A track whose file changed on the
    /// server (new part) is fetched again.
    pub(crate) fn refresh_kept_details(&mut self, album_key: &str, tracks: &[Track], cx: &mut Context<Self>) {
        let Some(a) = self.offline.index.albums.iter_mut().find(|a| a.key == album_key) else { return };
        let mut changed = false;
        let mut refetch = false;
        for k in &mut a.tracks {
            if let Some(t) = tracks.iter().find(|t| t.rating_key == k.rating_key) {
                if t.part_key != k.part_key && file_for(&t.part_key).is_some() {
                    // Keep the old file playable until the new one lands.
                    if k.prev_part.is_none() && file_for(&k.part_key).is_some_and(|p| p.exists()) {
                        k.prev_part = Some(k.part_key.clone());
                    }
                    k.part_key = t.part_key.clone();
                    k.title = t.title.clone();
                    refetch = true;
                    changed = true;
                }
                let fresh = KeptTrack::from_track(t);
                if (fresh.artist.as_str(), fresh.duration_ms, fresh.track_no, fresh.disc) != (k.artist.as_str(), k.duration_ms, k.track_no, k.disc) {
                    k.artist = fresh.artist;
                    k.duration_ms = fresh.duration_ms;
                    k.track_no = fresh.track_no;
                    k.disc = fresh.disc;
                    changed = true;
                }
            }
        }
        if refetch {
            a.complete = false;
            a.failed = None;
            let key = a.key.clone();
            if !self.offline.queue.contains(&key) {
                self.offline.queue.push_back(key);
            }
        }
        if changed {
            self.offline.reindex();
            self.save_offline();
        }
        if refetch {
            self.pump_offline(cx);
        }
    }

    /// Fill in details for kept albums whose index entries predate them
    /// (once, when the library has loaded from the server).
    /// Quietly, off the ordered track-list queue (so it never delays a
    /// click), and failures just leave the entry for next time.
    pub(crate) fn backfill_kept(&mut self, cx: &mut Context<Self>) {
        let stale: Vec<String> = self.offline.index.albums.iter().filter(|a| a.tracks.iter().any(|t| t.duration_ms == 0)).map(|a| a.key.clone()).collect();
        for key in stale {
            let k = key.clone();
            let rx = crate::plex_call(move |c| async move { c.tracks(&k).await });
            cx.spawn(async move |this, cx| {
                if let Ok(Ok(tracks)) = rx.await {
                    this.update(cx, |this, cx| this.refresh_kept_details(&key, &tracks, cx)).ok();
                }
            })
            .detach();
        }
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
                if !shared && let Some(p) = file_for(&t.part_key) {
                    let _ = std::fs::remove_file(crate::plex::partial_path(&p));
                    let _ = std::fs::remove_file(&p);
                }
            }
            self.save_offline();
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
                let Some(dest) = file_for(&t.part_key) else { continue };
                if !dest.exists() {
                    let (pk, b, c, d) = (t.part_key.clone(), bytes.clone(), cancel.clone(), dest.clone());
                    let mut rx = crate::plex_call(move |cl| async move { cl.download(&pk, &d, b, c).await });
                    // Refresh the progress display while the file comes in (GPUI
                    // timer: this task doesn't run on tokio).
                    let res = loop {
                        match rx.try_recv() {
                            Ok(r) => break Ok(r),
                            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                                cx.background_executor().timer(Duration::from_millis(250)).await;
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
                            failed = Some(format!("{}: download ended unexpectedly", t.title));
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
                let failed = failed.filter(|_| !cancelled);
                if let Some(a) = this.offline.index.albums.iter_mut().find(|a| a.key == key) {
                    a.complete = !a.tracks.is_empty() && a.tracks.iter().all(|t| file_for(&t.part_key).is_some_and(|p| p.exists()));
                    // Replaced files are no longer needed once their new
                    // ones are kept.
                    for t in a.tracks.iter_mut() {
                        if t.prev_part.is_some()
                            && file_for(&t.part_key).is_some_and(|p| p.exists())
                            && let Some(old) = t.prev_part.take().and_then(|p| file_for(&p))
                        {
                            let _ = std::fs::remove_file(old);
                        }
                    }
                    a.bytes = album_bytes(a);
                    a.failed = if a.complete { None } else { failed.clone() };
                }
                this.save_offline();
                if let Some(e) = failed {
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
