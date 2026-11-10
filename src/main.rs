//! Aulos GPUI spike: themed shell, virtualized album grid over a real Plex
//! library, cover art fetched on a tokio side thread, and a scroll benchmark.
//!
//! Env:
//!   PLEX_URL      (default http://plex-server:32400)
//!   PLEX_TOKEN    (required; set in the environment)
//!   PLEX_SECTION  (default 5)
//!   AULOS_ITEMS   (default 50000; real albums are cycled to fill)
//!   AULOS_BENCH   (set: auto-scroll, print frame stats, quit)

mod mpris;
mod plex;
mod theme;

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{
    App, Bounds, Context, Entity, Image, ImageFormat, ObjectFit, Pixels, Point, SharedString,
    Window, WindowBounds, WindowOptions, div, img, prelude::*, px, rgb, size, uniform_list,
    UniformListScrollHandle,
};
use gpui_component::input::{Input, InputState};
use tokio::sync::mpsc;

use aulos_spike::audio::{Engine, Event as AudioEvent, Source};
use mpris::{Transport, Update as MprisUpdate};
use plex::{Album, ArtRequest, ArtResult, HubRow, HubsRequest, SearchHit, SearchRequest, Track, TracksRequest};

gpui::actions!(aulos, [PlayPause, NextTrack, PrevTrack, SeekForward, SeekBack, Back, FocusSearch]);

enum View {
    Home,
    Library,
    Album { ix: usize, tracks: Option<Vec<Track>> },
}
use theme::Theme;

const CARD_W: f32 = 184.0;
const ART: f32 = 184.0;
const ROW_H: f32 = ART + 64.0;
const GAP: f32 = 20.0;
const SIDEBAR_W: f32 = 240.0;
const PAD_X: f32 = 36.0;

struct Aulos {
    theme: Theme,
    theme_gen: String,
    albums: Vec<Album>,
    total: usize,
    status: SharedString,
    art: HashMap<String, Arc<Image>>,
    requested: HashSet<String>,
    art_tx: mpsc::UnboundedSender<ArtRequest>,
    scroll: UniformListScrollHandle,
    search: Entity<InputState>,
    cols: usize,
    bench: Option<Bench>,
    last_render_us: u128,
    tracks_tx: mpsc::UnboundedSender<TracksRequest>,
    plex: (String, String),
    target: Option<String>,
    engine: Option<Engine>,
    now: Option<Now>,
    autoplay: Option<usize>,
    /// Bumped on every engine start so stale poll loops exit.
    play_gen: u64,
    view: View,
    album_scroll: gpui::ScrollHandle,
    search_tx: mpsc::UnboundedSender<SearchRequest>,
    search_gen: u64,
    search_query: String,
    hits: Vec<SearchHit>,
    /// Set after a hit is activated: next render clears the box and refocuses the app.
    reset_search: bool,
    home: Option<Vec<HubRow>>,
    mpris: tokio::sync::mpsc::UnboundedSender<MprisUpdate>,
    focus: gpui::FocusHandle,
}

struct Now {
    album: Album,
    tracks: Vec<Track>,
    /// index -> (source rate, bits), from engine TrackStarted events.
    formats: HashMap<usize, (u32, u32)>,
    /// Album track index the current engine started at, and the offset into it.
    start_idx: usize,
    start_ms: u64,
    /// Engine-relative track index -> start on the played-time clock (us).
    boundaries: Vec<u64>,
    idx: usize,
    elapsed_ms: u64,
    graph_rate: u32,
    paused: bool,
    finished: bool,
    sent_idx: Option<usize>,
}

struct Bench {
    started: Option<Instant>,
    last: Option<Instant>,
    intervals: Vec<f64>,
    render_us: Vec<u128>,
    y: f32,
    speed: f32, // px per second
    duration: Duration,
}

impl Aulos {
    #[allow(clippy::too_many_arguments)]
    fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        art_tx: mpsc::UnboundedSender<ArtRequest>,
        mut art_rx: mpsc::UnboundedReceiver<ArtResult>,
        mut albums_rx: mpsc::UnboundedReceiver<(&'static str, anyhow::Result<Vec<Album>>)>,
        tracks_tx: mpsc::UnboundedSender<TracksRequest>,
        plex: (String, String),
        mut transport_rx: mpsc::UnboundedReceiver<Transport>,
        mpris: mpsc::UnboundedSender<MprisUpdate>,
        search_tx: mpsc::UnboundedSender<SearchRequest>,
        hubs_tx: mpsc::UnboundedSender<HubsRequest>,
    ) -> Self {
        {
            let (reply, rx) = tokio::sync::oneshot::channel();
            let _ = hubs_tx.send(HubsRequest { reply });
            cx.spawn(async move |this, cx| match rx.await {
                Ok(Ok(rows)) => {
                    this.update(cx, |this, cx| {
                        for r in &rows {
                            for it in &r.items {
                                if !it.thumb.is_empty() && this.requested.insert(it.thumb.clone()) {
                                    let _ = this.art_tx.send(ArtRequest { thumb: it.thumb.clone() });
                                }
                            }
                        }
                        this.home = Some(rows);
                        cx.notify();
                    })
                    .ok();
                }
                Ok(Err(e)) => eprintln!("[aulos] hubs: {e:#}"),
                Err(_) => {}
            })
            .detach();
        }
        // MPRIS commands arrive from the mpris thread.
        cx.spawn(async move |this, cx| {
            while let Some(t) = transport_rx.recv().await {
                if this.update(cx, |this, cx| this.transport(t, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let (theme, theme_gen) = Theme::load();
        apply_component_theme(&theme, Some(window), cx);
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search, play, or run a command  (Ctrl K)"));
        cx.subscribe(&search, |this, input, ev: &gpui_component::input::InputEvent, cx| {
            use gpui_component::input::InputEvent;
            match ev {
                InputEvent::Change => {
                    let q = input.read(cx).value().to_string();
                    this.on_search_change(q, cx);
                }
                InputEvent::PressEnter { .. } => {
                    if let Some(h) = this.hits.first().cloned() {
                        this.activate_hit(h, cx);
                    }
                }
                _ => {}
            }
        })
        .detach();

        // Albums arrive from the tokio side thread; awaited on GPUI's executor.
        // First from the disk cache (if any), then fresh from Plex.
        cx.spawn(async move |this, cx| {
            let mut first = true;
            while let Some((from, res)) = albums_rx.recv().await {
            let ok = this.update(cx, |this, cx| {
                match res {
                    Ok(albums) => {
                        if !first && albums == this.albums {
                            this.status = format!("{} albums · up to date", albums.len()).into();
                            cx.notify();
                            return;
                        }
                        this.status = format!("{} albums ({from})", albums.len()).into();
                        // Keep an open album page pointing at the same album.
                        let open_key = match &this.view {
                            View::Album { ix, .. } => this.albums.get(*ix).map(|a| a.rating_key.clone()),
                            _ => None,
                        };
                        this.albums = albums;
                        if let Some(key) = open_key
                            && let View::Album { ix, .. } = &mut this.view
                        {
                            if let Some(n) = this.albums.iter().position(|a| a.rating_key == key) {
                                *ix = n;
                            }
                        }
                        if !std::mem::replace(&mut first, false) {
                            cx.notify();
                            return;
                        }
                        if let Some(ix) = this.autoplay.take() {
                            this.play_album(ix, cx);
                        } else if let Ok(key) = std::env::var("AULOS_OPEN_KEY")
                            && let Some(ix) = this.albums.iter().position(|a| a.rating_key == key) {
                                this.open_album(ix, cx);
                        } else if let Ok(key) = std::env::var("AULOS_AUTOPLAY_KEY")
                            && let Some(ix) = this.albums.iter().position(|a| a.rating_key == key) {
                                this.play_album(ix, cx);
                            }
                    }
                    Err(e) => this.status = format!("Plex error: {e:#}").into(),
                }
                cx.notify();
            });
            if ok.is_err() {
                break;
            }
            }
        })
        .detach();

        // Art results stream back the same way.
        cx.spawn(async move |this, cx| {
            while let Some(res) = art_rx.recv().await {
                let ok = this
                    .update(cx, |this, cx| {
                        if let Some(bytes) = res.bytes {
                            let fmt = if bytes.starts_with(&[0x89, b'P']) {
                                ImageFormat::Png
                            } else {
                                ImageFormat::Jpeg
                            };
                            this.art
                                .insert(res.thumb, Arc::new(Image::from_bytes(fmt, bytes)));
                            cx.notify();
                        }
                    })
                    .is_ok();
                if !ok {
                    break;
                }
            }
        })
        .detach();

        // Theme hot reload: Omarchy bumps theme.generation on every switch.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let gen_now = theme::generation();
                let ok = this
                    .update(cx, |this, cx| {
                        if gen_now != this.theme_gen {
                            let (t, g) = Theme::load();
                            eprintln!("[aulos] theme changed -> accent {:06x}", t.accent);
                            apply_component_theme(&t, None, cx);
                            this.theme = t;
                            this.theme_gen = g;
                            cx.notify();
                        }
                    })
                    .is_ok();
                if !ok {
                    break;
                }
            }
        })
        .detach();

        let total = std::env::var("AULOS_ITEMS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0); // 0 = the real library size
        let bench = std::env::var("AULOS_BENCH").ok().map(|_| Bench {
            started: None,
            last: None,
            intervals: Vec::new(),
            render_us: Vec::new(),
            y: 0.0,
            speed: 3000.0,
            duration: Duration::from_secs(12),
        });

        Self {
            theme,
            theme_gen,
            albums: Vec::new(),
            total,
            status: "Loading library…".into(),
            art: HashMap::new(),
            requested: HashSet::new(),
            art_tx,
            scroll: UniformListScrollHandle::new(),
            search,
            cols: 6,
            bench,
            last_render_us: 0,
            tracks_tx,
            plex,
            target: std::env::var("AULOS_TARGET").ok(),
            engine: None,
            now: None,
            autoplay: std::env::var("AULOS_AUTOPLAY").ok().and_then(|v| v.parse().ok()),
            play_gen: 0,
            view: View::Home,
            album_scroll: gpui::ScrollHandle::new(),
            search_tx,
            search_gen: 0,
            search_query: String::new(),
            hits: Vec::new(),
            reset_search: false,
            home: None,
            mpris,
            focus,
        }
    }

    fn on_search_change(&mut self, q: String, cx: &mut Context<Self>) {
        self.search_query = q.trim().to_string();
        self.search_gen += 1;
        let gen_id = self.search_gen;
        if self.search_query.is_empty() {
            self.hits.clear();
            cx.notify();
            return;
        }
        let query = self.search_query.clone();
        let tx = self.search_tx.clone();
        cx.spawn(async move |this, cx| {
            // Debounce: only the latest keystroke after 120 ms queries Plex.
            cx.background_executor().timer(Duration::from_millis(120)).await;
            if this.update(cx, |this, _| this.search_gen != gen_id).unwrap_or(true) {
                return;
            }
            let (reply, rx) = tokio::sync::oneshot::channel();
            let t0 = Instant::now();
            let _ = tx.send(SearchRequest { query: query.clone(), reply });
            if let Ok(Ok(hits)) = rx.await {
                eprintln!("[aulos] search {query:?}: {} hits in {:?}", hits.len(), t0.elapsed());
                this.update(cx, |this, cx| {
                    if this.search_gen == gen_id {
                        for h in &hits {
                            if !h.thumb.is_empty() && !this.art.contains_key(&h.thumb) && this.requested.insert(h.thumb.clone()) {
                                let _ = this.art_tx.send(ArtRequest { thumb: h.thumb.clone() });
                            }
                        }
                        this.hits = hits;
                        cx.notify();
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    fn activate_hit(&mut self, h: SearchHit, cx: &mut Context<Self>) {
        eprintln!("[aulos] activate {} {:?}", h.kind, h.title);
        match h.kind.as_str() {
            "album" => {
                if let Some(ix) = self.albums.iter().position(|a| a.rating_key == h.rating_key) {
                    self.open_album(ix, cx);
                }
            }
            "artist" => {
                if let Some(ix) = self.albums.iter().position(|a| a.artist == h.title) {
                    self.open_album(ix, cx);
                }
            }
            "track" => {
                if let Some(album) = self.albums.iter().find(|a| a.rating_key == h.parent_key).cloned() {
                    let key = h.rating_key.clone();
                    self.request_art(&album);
                    self.with_tracks(album, cx, move |this, album, tracks, cx| {
                        let start = tracks.iter().position(|t| t.rating_key == key).unwrap_or(0);
                        this.play_tracks(album, tracks, start, cx);
                    });
                }
            }
            _ => {}
        }
        self.search_query.clear();
        self.hits.clear();
        self.reset_search = true;
        cx.notify();
    }

    fn request_art(&mut self, album: &Album) {
        if !album.thumb.is_empty() && !self.art.contains_key(&album.thumb) && self.requested.insert(album.thumb.clone()) {
            let _ = self.art_tx.send(ArtRequest { thumb: album.thumb.clone() });
        }
    }

    /// Fetch an album's tracks, then hand them to `f` on the UI thread.
    fn with_tracks(
        &mut self,
        album: Album,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Self, Album, Vec<Track>, &mut Context<Self>) + 'static,
    ) {
        let (reply, rx) = tokio::sync::oneshot::channel();
        let _ = self.tracks_tx.send(TracksRequest { album_key: album.rating_key.clone(), reply });
        cx.spawn(async move |this, cx| {
            match rx.await {
                Ok(Ok(t)) => {
                    this.update(cx, |this, cx| f(this, album, t, cx)).ok();
                }
                Ok(Err(e)) => eprintln!("[aulos] tracks: {e:#}"),
                Err(_) => {}
            }
        })
        .detach();
    }

    fn open_album(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(album) = self.album(ix).cloned() else { return };
        self.request_art(&album);
        self.view = View::Album { ix, tracks: None };
        self.album_scroll = gpui::ScrollHandle::new();
        cx.notify();
        self.with_tracks(album, cx, move |this, _album, tracks, cx| {
            if let View::Album { ix: cur, tracks: slot } = &mut this.view
                && *cur == ix {
                    // Test hook: behave as if track N (1-based) was clicked.
                    let auto = std::env::var("AULOS_PLAY_TRACK").ok().and_then(|v| v.parse::<usize>().ok());
                    *slot = Some(tracks.clone());
                    if let (Some(n), Some(album)) = (auto, this.album(ix).cloned()) {
                        this.play_tracks(album, tracks, n.saturating_sub(1), cx);
                    }
                    cx.notify();
                }
        });
    }

    fn play_album(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(album) = self.album(ix).cloned() else { return };
        self.request_art(&album);
        self.status = format!("Loading {}…", album.title).into();
        cx.notify();
        self.with_tracks(album, cx, |this, album, tracks, cx| {
            if !tracks.is_empty() {
                this.play_tracks(album, tracks, 0, cx);
            }
        });
    }

    fn play_tracks(&mut self, album: Album, tracks: Vec<Track>, start: usize, cx: &mut Context<Self>) {
        self.write_art_for_mpris(&album);
        self.now = Some(Now {
            album,
            tracks,
            formats: HashMap::new(),
            start_idx: 0,
            start_ms: 0,
            boundaries: Vec::new(),
            idx: start,
            elapsed_ms: 0,
            graph_rate: 0,
            paused: false,
            finished: false,
            sent_idx: None,
        });
        self.play_from(start, 0, cx);
    }

    fn art_path(album: &Album) -> std::path::PathBuf {
        std::path::PathBuf::from(format!("/scratch/aulos-cache/art-{}.jpg", album.rating_key))
    }

    fn write_art_for_mpris(&self, album: &Album) {
        if let Some(img) = self.art.get(&album.thumb) {
            let _ = std::fs::write(Self::art_path(album), img.bytes());
        }
    }

    /// (Re)start the engine at album track `idx`, `ms` into it.
    fn play_from(&mut self, idx: usize, ms: u64, cx: &mut Context<Self>) {
        let Some(now) = self.now.as_mut() else { return };
        if idx >= now.tracks.len() {
            return;
        }
        self.engine = None; // dropping stops the old one
        let (base, token) = self.plex.clone();
        let queue = now.tracks[idx..]
            .iter()
            .map(|t| {
                let id = t.part_key.split('/').nth(3).unwrap_or("x").to_string();
                let ext = t.part_key.rsplit('.').next().unwrap_or("bin").to_string();
                Source::Http {
                    url: format!("{base}{}", t.part_key),
                    token: token.clone(),
                    cache: std::path::PathBuf::from(format!("/scratch/aulos-cache/part-{id}.{ext}")),
                }
            })
            .collect();
        match Engine::play_from(queue, ms, self.target.clone()) {
            Ok(e) => {
                self.engine = Some(e);
                now.start_idx = idx;
                now.start_ms = ms;
                now.boundaries.clear();
                now.idx = idx;
                now.elapsed_ms = ms;
                now.paused = false;
                now.finished = false;
                self.status = format!("Playing {}", now.album.title).into();
                let _ = self.mpris.send(MprisUpdate::Status { playing: true, stopped: false });
                if ms > 0 {
                    let _ = self.mpris.send(MprisUpdate::Seeked(ms));
                }
            }
            Err(e) => self.status = format!("Audio error: {e:#}").into(),
        }
        self.play_gen += 1;
        let gen_id = self.play_gen;
        // Poll only while this engine is current (idle stays ~0%).
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(250)).await;
                let keep = this
                    .update(cx, |this, cx| this.play_gen == gen_id && this.poll_engine(cx))
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }

    fn transport(&mut self, t: Transport, cx: &mut Context<Self>) {
        let Some(now) = self.now.as_mut() else { return };
        let (idx, elapsed, n) = (now.idx, now.elapsed_ms, now.tracks.len());
        eprintln!("[aulos] transport {t:?}");
        match t {
            Transport::PlayPause | Transport::Play | Transport::Pause => {
                let want_pause = match t {
                    Transport::Play => false,
                    Transport::Pause => true,
                    _ => !now.paused,
                };
                if now.finished {
                    if !want_pause {
                        self.play_from(0, 0, cx);
                    }
                    return;
                }
                now.paused = want_pause;
                if let Some(e) = &self.engine {
                    e.set_active(!want_pause);
                }
                let _ = self.mpris.send(MprisUpdate::Status { playing: !want_pause, stopped: false });
                cx.notify();
            }
            Transport::Next => {
                if idx + 1 < n {
                    self.play_from(idx + 1, 0, cx);
                }
            }
            Transport::Prev => {
                if elapsed > 3000 || idx == 0 {
                    self.play_from(idx, 0, cx);
                } else {
                    self.play_from(idx - 1, 0, cx);
                }
            }
            Transport::Seek(off) => {
                let len = now.tracks[idx].duration_ms;
                let to = (elapsed as i64 + off).clamp(0, len.saturating_sub(1000) as i64) as u64;
                self.play_from(idx, to, cx);
            }
            Transport::SetPosition(ms) => self.play_from(idx, ms, cx),
            Transport::Stop => {
                self.engine = None;
                now.finished = true;
                now.paused = true;
                let _ = self.mpris.send(MprisUpdate::Status { playing: false, stopped: true });
                cx.notify();
            }
        }
    }

    /// Returns false once playback is over.
    fn poll_engine(&mut self, cx: &mut Context<Self>) -> bool {
        let (Some(engine), Some(now)) = (self.engine.as_ref(), self.now.as_mut()) else { return false };
        while let Ok(ev) = engine.events.try_recv() {
            match ev {
                AudioEvent::TrackStarted { index, source_rate, bits, at_us, .. } => {
                    now.formats.insert(now.start_idx + index, (source_rate, bits));
                    if now.boundaries.len() == index {
                        now.boundaries.push(at_us);
                    }
                }
                AudioEvent::Reconnected { from, to, silent_ms } => {
                    eprintln!("[aulos] rate change {from} -> {to}: {silent_ms:.1} ms");
                }
                AudioEvent::Finished => now.finished = true,
                AudioEvent::Error(e) => {
                    eprintln!("[aulos] audio: {e}");
                    now.finished = true;
                }
                AudioEvent::FirstAudio { ms_since_play } => {
                    eprintln!("[aulos] first audio {ms_since_play:.0} ms");
                }
            }
        }
        let played_us = engine.stats.played_us.load(std::sync::atomic::Ordering::Relaxed);
        now.graph_rate = engine.stats.graph_rate.load(std::sync::atomic::Ordering::Relaxed);
        // Current track: the last engine boundary at or before the played clock.
        let rel = now.boundaries.iter().rposition(|&b| b <= played_us).unwrap_or(0);
        let base = now.boundaries.get(rel).copied().unwrap_or(0);
        now.idx = (now.start_idx + rel).min(now.tracks.len() - 1);
        now.elapsed_ms = (played_us - base.min(played_us)) / 1000 + if rel == 0 { now.start_ms } else { 0 };

        // Cover arrived after the track was announced: write it and re-announce.
        let art = Self::art_path(&now.album);
        if !art.exists()
            && let Some(img) = self.art.get(&now.album.thumb)
                && std::fs::write(&art, img.bytes()).is_ok() {
                    now.sent_idx = None;
                }
        if now.sent_idx != Some(now.idx) {
            now.sent_idx = Some(now.idx);
            let tr = &now.tracks[now.idx];
            let art = Self::art_path(&now.album);
            let _ = self.mpris.send(MprisUpdate::Track {
                id: tr.part_key.split('/').nth(3).unwrap_or("0").to_string(),
                title: tr.title.clone(),
                artist: tr.artist.clone(),
                album: now.album.title.clone(),
                length_ms: tr.duration_ms,
                art_url: art.exists().then(|| format!("file://{}", art.display())),
            });
        }
        let _ = self.mpris.send(MprisUpdate::Position(now.elapsed_ms));
        if now.finished {
            let _ = self.mpris.send(MprisUpdate::Status { playing: false, stopped: true });
        }
        cx.notify();
        !now.finished
    }

    /// Grid cells: the real library, or AULOS_ITEMS for the scroll benchmark.
    fn cells(&self) -> usize {
        if self.total == 0 { self.albums.len() } else { self.total }
    }

    fn album(&self, ix: usize) -> Option<&Album> {
        if self.albums.is_empty() {
            None
        } else {
            Some(&self.albums[ix % self.albums.len()])
        }
    }

    fn bench_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let render_us = self.last_render_us;
        let cells = self.cells();
        let Some(b) = self.bench.as_mut() else { return };
        // Wait for the library and the first screen of art before measuring.
        if self.albums.is_empty() || self.art.len() < 12 {
            window.request_animation_frame();
            return;
        }
        let now = Instant::now();
        if b.started.is_none() {
            b.started = Some(now);
            eprintln!("[aulos] bench: scrolling at {} px/s for {:?}", b.speed, b.duration);
        }
        if let Some(last) = b.last {
            let dt = now.duration_since(last);
            b.intervals.push(dt.as_secs_f64() * 1000.0);
            b.render_us.push(render_us);
            b.y += b.speed * dt.as_secs_f32();
        }
        b.last = Some(now);
        let max_y = (cells.div_ceil(self.cols.max(1)) as f32) * ROW_H;
        if b.y > max_y {
            b.y = 0.0;
        }
        self.scroll
            .0
            .borrow()
            .base_handle
            .set_offset(Point::new(px(0.0), px(-b.y)));

        if now.duration_since(b.started.unwrap()) >= b.duration {
            report(b, self.art.len());
            cx.quit();
            return;
        }
        window.request_animation_frame();
        cx.notify();
    }

    fn row(&mut self, row: usize, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let t = self.theme.clone();
        let mut cards = Vec::with_capacity(self.cols);
        for c in 0..self.cols {
            let ix = row * self.cols + c;
            if ix >= self.cells() {
                break;
            }
            let Some(a) = self.album(ix).cloned() else { break };
            let art = self.art.get(&a.thumb).cloned();
            if art.is_none() && !a.thumb.is_empty() && self.requested.insert(a.thumb.clone()) {
                let _ = self.art_tx.send(ArtRequest { thumb: a.thumb.clone() });
            }
            let cover = match art {
                Some(image) => img(image)
                    .size(px(ART))
                    .rounded(px(8.0))
                    .object_fit(ObjectFit::Cover)
                    .into_any_element(),
                None => div()
                    .size(px(ART))
                    .rounded(px(8.0))
                    .bg(rgb(t.raised))
                    .into_any_element(),
            };
            cards.push(
                div()
                    .id(ix)
                    .w(px(CARD_W))
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .cursor_pointer()
                    .child(cover)
                    .child(
                        div()
                            .text_size(px(14.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(t.text_strong))
                            .truncate()
                            .child(a.title.clone()),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .text_size(px(12.0))
                            .text_color(rgb(t.muted))
                            .child(div().truncate().child(a.artist.clone()))
                            .child(a.year.clone()),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_album(ix, cx);
                    })),
            );
        }
        div()
            .h(px(ROW_H))
            .px(px(PAD_X))
            .flex()
            .gap(px(GAP))
            .children(cards)
    }
}

/// Map Omarchy colors onto gpui-component's theme so its controls match.
fn apply_component_theme(t: &Theme, window: Option<&mut Window>, cx: &mut App) {
    use gpui_component::theme::{Theme as CTheme, ThemeMode};
    CTheme::change(ThemeMode::Dark, window, cx);
    let c = |x: u32| -> gpui::Hsla { rgb(x).into() };
    CTheme::update(cx, |th| {
        let k = &mut th.colors;
        k.background = c(t.base);
        k.foreground = c(t.text);
        k.input = c(t.border);
        k.border = c(t.border);
        k.ring = c(t.accent);
        k.caret = c(t.accent);
        k.primary = c(t.accent);
        k.primary_foreground = c(t.base);
        k.muted = c(t.raised);
        k.muted_foreground = c(t.muted);
        k.accent = c(t.raised);
        k.accent_foreground = c(t.text_strong);
        k.popover = c(t.surface);
        k.popover_foreground = c(t.text);
        k.secondary = c(t.surface);
        k.secondary_foreground = c(t.text);
        k.list = c(t.surface);
        k.list_hover = c(t.raised);
        k.list_active = c(t.raised);
        k.scrollbar_thumb = c(t.border);
    });
}

impl Aulos {
    fn home_view(&self, t: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut col = div()
            .id("home")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px(px(PAD_X))
            .pt(px(8.0))
            .pb(px(40.0))
            .flex()
            .flex_col()
            .gap(px(32.0))
            .child(div().text_size(px(34.0)).font_weight(gpui::FontWeight::BOLD).text_color(rgb(t.text_strong)).child("Home"));
        let Some(rows) = &self.home else {
            return col.child(div().text_color(rgb(t.muted)).child("Loading…")).into_any_element();
        };
        // Wider than the window clips; show as many as fit.
        let fit = self.cols.max(1);
        for (ri, row) in rows.iter().enumerate() {
            let station = row.kind == "station";
            let cards: Vec<gpui::AnyElement> = row
                .items
                .iter()
                .take(if station { 8 } else { fit })
                .enumerate()
                .map(|(ci, it)| {
                    let art = self.art.get(&it.thumb).cloned();
                    let round = if row.kind == "artist" { ART / 2.0 } else { 8.0 };
                    let size = if station { 72.0 } else { ART };
                    let cover = match art {
                        Some(i) => img(i).size(px(size)).flex_none().rounded(px(round)).object_fit(ObjectFit::Cover).into_any_element(),
                        None => div()
                            .size(px(size))
                            .flex_none()
                            .rounded(px(if station { 36.0 } else { round }))
                            .bg(rgb(if station { t.accent } else { t.raised }))
                            .into_any_element(),
                    };
                    let (kind, key, title) = (row.kind.clone(), it.rating_key.clone(), it.title.clone());
                    let card = div()
                        .id(("hub", ri * 100 + ci))
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| this.open_hub_item(&kind, &key, &title, cx)));
                    if station {
                        card.h(px(84.0))
                            .px(px(12.0))
                            .flex()
                            .items_center()
                            .gap(px(12.0))
                            .rounded(px(12.0))
                            .border_1()
                            .border_color(rgb(t.border))
                            .bg(rgb(t.surface))
                            .hover(|d| d.bg(rgb(t.raised)))
                            .child(div().size(px(44.0)).flex_none().rounded(px(22.0)).bg(rgb(t.accent)))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .child(div().text_color(rgb(t.text_strong)).font_weight(gpui::FontWeight::SEMIBOLD).child(it.title.clone()))
                                    .child(div().text_size(px(12.0)).text_color(rgb(t.muted)).child("Station · coming soon")),
                            )
                            .into_any_element()
                    } else {
                        card.w(px(CARD_W))
                            .flex()
                            .flex_col()
                            .gap(px(8.0))
                            .child(cover)
                            .child(div().text_size(px(14.0)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(rgb(t.text_strong)).truncate().child(it.title.clone()))
                            .child(div().text_size(px(12.0)).text_color(rgb(t.muted)).truncate().child(it.subtitle.clone()))
                            .into_any_element()
                    }
                })
                .collect();
            col = col.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(14.0))
                    .child(div().text_size(px(20.0)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(rgb(t.text_strong)).child(row.title.clone()))
                    .child(div().flex().flex_wrap().gap(px(GAP)).children(cards)),
            );
        }
        col.into_any_element()
    }

    fn open_hub_item(&mut self, kind: &str, key: &str, title: &str, cx: &mut Context<Self>) {
        match kind {
            "album" => {
                if let Some(ix) = self.albums.iter().position(|a| a.rating_key == key) {
                    self.open_album(ix, cx);
                }
            }
            "artist" => {
                if let Some(ix) = self.albums.iter().position(|a| a.artist == title) {
                    self.open_album(ix, cx);
                }
            }
            _ => eprintln!("[aulos] station {title:?} not implemented yet"),
        }
    }

    fn search_view(&self, t: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut col = div()
            .id("search-results")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px(px(PAD_X))
            .pb(px(32.0))
            .flex()
            .flex_col()
            .gap(px(4.0));
        if self.hits.is_empty() {
            col = col.child(div().text_color(rgb(t.muted)).child(format!("Searching for {:?}…", self.search_query)));
        }
        let mut last_kind = String::new();
        for (i, h) in self.hits.iter().enumerate() {
            if h.kind != last_kind {
                last_kind = h.kind.clone();
                let label = match h.kind.as_str() { "artist" => "Artists", "album" => "Albums", _ => "Tracks" };
                col = col.child(div().mt(px(14.0)).mb(px(4.0)).text_size(px(12.0)).text_color(rgb(t.muted)).child(label));
            }
            let thumb = match self.art.get(&h.thumb).cloned() {
                Some(i) => img(i).size(px(40.0)).flex_none().rounded(px(if h.kind == "artist" { 20.0 } else { 4.0 })).object_fit(ObjectFit::Cover).into_any_element(),
                None => div().size(px(40.0)).flex_none().rounded(px(4.0)).bg(rgb(t.raised)).into_any_element(),
            };
            let hit = h.clone();
            col = col.child(
                div()
                    .id(("hit", i))
                    .h(px(52.0))
                    .px(px(10.0))
                    .flex()
                    .items_center()
                    .gap(px(14.0))
                    .rounded(px(8.0))
                    .cursor_pointer()
                    .when(i == 0, |d| d.bg(rgb(t.raised)))
                    .hover(|d| d.bg(rgb(t.raised)))
                    .child(thumb)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .min_w_0()
                            .child(div().text_color(rgb(t.text_strong)).truncate().child(h.title.clone()))
                            .child(div().text_size(px(12.0)).text_color(rgb(t.muted)).truncate().child(h.subtitle.clone())),
                    )
                    .child(div().flex_1())
                    .when(i == 0, |d| d.child(div().text_size(px(11.0)).text_color(rgb(t.muted)).child("Enter")))
                    .on_click(cx.listener(move |this, _, _, cx| this.activate_hit(hit.clone(), cx))),
            );
        }
        col.into_any_element()
    }

    fn album_view(&self, ix: usize, tracks: Option<&Vec<Track>>, t: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(album) = self.album(ix).cloned() else { return div().into_any_element() };
        let cover = match self.art.get(&album.thumb).cloned() {
            Some(i) => img(i).size(px(232.0)).flex_none().rounded(px(10.0)).object_fit(ObjectFit::Cover).into_any_element(),
            None => div().size(px(232.0)).flex_none().rounded(px(10.0)).bg(rgb(t.raised)).into_any_element(),
        };
        let playing_here = self.now.as_ref().filter(|n| n.album.rating_key == album.rating_key).map(|n| n.idx);
        let total_ms: u64 = tracks.map(|v| v.iter().map(|t| t.duration_ms).sum()).unwrap_or(0);
        let meta = match tracks {
            Some(v) => format!("{} · {} tracks · {} min", album.artist, v.len(), total_ms / 60_000),
            None => format!("{} · loading tracks…", album.artist),
        };
        let play_btn = div()
            .id("album-play")
            .h(px(40.0))
            .px(px(22.0))
            .flex()
            .items_center()
            .rounded(px(20.0))
            .bg(rgb(t.accent))
            .text_color(rgb(t.base))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .cursor_pointer()
            .child("Play")
            .on_click(cx.listener(move |this, _, _, cx| {
                if let View::Album { tracks: Some(tr), .. } = &this.view
                    && let Some(a) = this.album(ix).cloned() {
                        let tr = tr.clone();
                        this.play_tracks(a, tr, 0, cx);
                    }
            }));
        let back_btn = div()
            .id("album-back")
            .h(px(40.0))
            .px(px(16.0))
            .flex()
            .items_center()
            .rounded(px(20.0))
            .border_1()
            .border_color(rgb(t.border))
            .text_color(rgb(t.text))
            .cursor_pointer()
            .child("Back")
            .on_click(cx.listener(|this, _, _, cx| {
                this.view = View::Library;
                cx.notify();
            }));
        let header = div()
            .flex()
            .gap(px(28.0))
            .items_end()
            .child(cover)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .min_w_0()
                    .child(div().text_size(px(12.0)).text_color(rgb(t.muted)).child(format!("ALBUM · {}", album.year)))
                    .child(
                        div()
                            .text_size(px(40.0))
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(rgb(t.text_strong))
                            .child(album.title.clone()),
                    )
                    .child(div().text_size(px(15.0)).text_color(rgb(t.text)).child(meta))
                    .child(div().flex().gap(px(10.0)).mt(px(8.0)).child(play_btn).child(back_btn)),
            );
        let rows: Vec<gpui::AnyElement> = tracks
            .map(|v| {
                v.iter()
                    .enumerate()
                    .map(|(i, tr)| {
                        let playing = playing_here == Some(i);
                        let color = if playing { t.accent } else { t.text };
                        div()
                            .id(("track", i))
                            .h(px(44.0))
                            .px(px(12.0))
                            .flex()
                            .items_center()
                            .gap(px(16.0))
                            .rounded(px(8.0))
                            .cursor_pointer()
                            .when(playing, |d| d.bg(rgb(t.raised)))
                            .hover(|d| d.bg(rgb(t.raised)))
                            .child(div().w(px(28.0)).text_size(px(13.0)).text_color(rgb(if playing { t.accent } else { t.muted })).child(format!("{}", i + 1)))
                            .child(div().flex_1().min_w_0().truncate().text_color(rgb(color)).child(tr.title.clone()))
                            .child(div().text_size(px(13.0)).text_color(rgb(t.muted)).child(fmt_ms(tr.duration_ms)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let View::Album { tracks: Some(tr), .. } = &this.view
                                    && let Some(a) = this.album(ix).cloned() {
                                        let tr = tr.clone();
                                        this.play_tracks(a, tr, i, cx);
                                    }
                            }))
                            .into_any_element()
                    })
                    .collect()
            })
            .unwrap_or_default();
        div()
            .id("album-view")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.album_scroll)
            .px(px(PAD_X))
            .pt(px(28.0))
            .pb(px(40.0))
            .flex()
            .flex_col()
            .gap(px(24.0))
            .child(header)
            .child(div().flex().flex_col().children(rows))
            .into_any_element()
    }
}

fn fmt_ms(ms: u64) -> String {
    format!("{}:{:02}", ms / 60_000, (ms / 1000) % 60)
}

impl Aulos {
    fn player_bar(&self, t: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let (title, sub, art, frac, times, badge) = match &self.now {
            Some(n) => {
                let tr = &n.tracks[n.idx];
                let frac = if tr.duration_ms > 0 {
                    (n.elapsed_ms as f32 / tr.duration_ms as f32).min(1.0)
                } else {
                    0.0
                };
                let badge = n.formats.get(&n.idx).map(|&(rate, bits)| {
                    let khz = |r: u32| {
                        if r.is_multiple_of(1000) { format!("{}", r / 1000) } else { format!("{:.1}", r as f32 / 1000.0) }
                    };
                    if n.graph_rate == 0 {
                        (format!("{bits}/{}", khz(rate)), t.muted)
                    } else if n.graph_rate == rate {
                        (format!("{bits}/{} · native rate", khz(rate)), t.accent)
                    } else {
                        (format!("{bits}/{} → {} kHz · resampled by PipeWire", khz(rate), khz(n.graph_rate)), t.muted)
                    }
                });
                (
                    SharedString::from(tr.title.clone()),
                    SharedString::from(format!("{} · {} · track {} of {}", tr.artist, n.album.title, n.idx + 1, n.tracks.len())),
                    self.art.get(&n.album.thumb).cloned(),
                    frac,
                    Some((fmt_ms(n.elapsed_ms), fmt_ms(tr.duration_ms))),
                    badge,
                )
            }
            None => ("Nothing playing".into(), "Click an album to play it".into(), None, 0.0, None, None),
        };
        let paused = self.now.as_ref().is_none_or(|n| n.paused || n.finished);
        let len_ms = self.now.as_ref().map(|n| n.tracks[n.idx].duration_ms).unwrap_or(0);
        let btn = |id: &'static str, label: &'static str, tr: Transport, cx: &mut Context<Self>| {
            div()
                .id(id)
                .px(px(12.0))
                .h(px(32.0))
                .flex()
                .items_center()
                .rounded(px(16.0))
                .border_1()
                .border_color(rgb(t.border))
                .text_size(px(12.0))
                .text_color(rgb(t.text))
                .cursor_pointer()
                .hover(|d| d.bg(rgb(t.raised)))
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| this.transport(tr, cx)))
        };
        let controls = div()
            .flex()
            .gap(px(8.0))
            .child(btn("prev", "Prev", Transport::Prev, cx))
            .child(btn("pp", if paused { "Play" } else { "Pause" }, Transport::PlayPause, cx))
            .child(btn("next", "Next", Transport::Next, cx));
        // Seek bar: 120 click targets, each jumping to its share of the track.
        const SEG: usize = 120;
        let segs: Vec<_> = (0..SEG)
            .map(|i| {
                let ms = len_ms * i as u64 / SEG as u64;
                let filled = (i as f32 + 0.5) / SEG as f32 <= frac;
                div()
                    .id(("seek", i))
                    .flex_1()
                    .h(px(16.0))
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .child(div().w_full().h(px(4.0)).bg(rgb(if filled { t.accent } else { t.border })))
                    .on_click(cx.listener(move |this, _, _, cx| this.transport(Transport::SetPosition(ms), cx)))
            })
            .collect();
        let cover = match art {
            Some(i) => img(i).size(px(56.0)).flex_none().rounded(px(6.0)).object_fit(ObjectFit::Cover).into_any_element(),
            None => div().size(px(56.0)).flex_none().rounded(px(6.0)).bg(rgb(t.raised)).into_any_element(),
        };
        div()
            .h(px(88.0))
            .flex_none()
            .bg(rgb(t.surface))
            .border_t_1()
            .border_color(rgb(t.border))
            .px(px(24.0))
            .flex()
            .items_center()
            .gap(px(16.0))
            .child(cover)
            .child(
                div()
                    .w(px(360.0))
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(div().text_color(rgb(t.text_strong)).truncate().child(title))
                    .child(div().text_size(px(12.0)).text_color(rgb(t.muted)).truncate().child(sub)),
            )
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .text_size(px(12.0))
                    .text_color(rgb(t.muted))
                    .children(times.as_ref().map(|(a, _)| a.clone()))
                    .child(div().w(px(480.0)).flex().children(segs))
                    .children(times.map(|(_, b)| b)),
            )
            .child(controls)
            .child(div().flex_1())
            .children(badge.map(|(text, color)| {
                div()
                    .px(px(10.0))
                    .h(px(28.0))
                    .flex()
                    .items_center()
                    .rounded(px(14.0))
                    .border_1()
                    .border_color(rgb(color))
                    .text_size(px(12.0))
                    .text_color(rgb(color))
                    .child(text)
            }))
            .into_any_element()
    }
}

fn report(b: &Bench, art_loaded: usize) {
    let mut v = b.intervals.clone();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let pct = |p: f64| v[((v.len() as f64 - 1.0) * p).round() as usize];
    let secs = b.intervals.iter().sum::<f64>() / 1000.0;
    let budget = 1000.0 / 120.0;
    let missed = v.iter().filter(|&&d| d > budget * 1.5).count();
    let mut r = b.render_us.clone();
    r.sort();
    let rp = |p: f64| r[((r.len() as f64 - 1.0) * p).round() as usize];
    println!(
        "BENCH frames={} fps={:.1} interval_ms p50={:.2} p95={:.2} p99={:.2} max={:.2} \
         over_1.5x_120hz_budget={} ({:.2}%) render_us p50={} p99={} max={} art_loaded={}",
        v.len(),
        v.len() as f64 / secs,
        pct(0.50),
        pct(0.95),
        pct(0.99),
        v.last().unwrap(),
        missed,
        100.0 * missed as f64 / v.len() as f64,
        rp(0.5),
        rp(0.99),
        r.last().unwrap(),
        art_loaded
    );
}

impl Render for Aulos {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t0 = Instant::now();
        let t = self.theme.clone();
        if std::mem::take(&mut self.reset_search) {
            self.search.update(cx, |s, cx| s.set_value("", window, cx));
            self.search_query.clear();
            window.focus(&self.focus, cx);
        }
        let vw: f32 = window.viewport_size().width.into();
        let usable = (vw - SIDEBAR_W - PAD_X * 2.0).max(CARD_W);
        self.cols = (((usable + GAP) / (CARD_W + GAP)).floor() as usize).max(1);
        let rows = self.cells().div_ceil(self.cols);

        if self.bench.is_some() {
            self.bench_tick(window, cx);
        }

        let nav = ["Home", "Library", "Stations", "Playlists", "Downloads"];
        let sidebar = div()
            .w(px(SIDEBAR_W))
            .h_full()
            .flex_none()
            .bg(rgb(t.surface))
            .border_r_1()
            .border_color(rgb(t.border))
            .p(px(20.0))
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(
                div()
                    .text_size(px(22.0))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(rgb(t.text_strong))
                    .mb(px(16.0))
                    .child("Aulos"),
            )
            .children(nav.iter().enumerate().map(|(i, n)| {
                let active = match self.view {
                    View::Home => i == 0,
                    View::Library | View::Album { .. } => i == 1,
                };
                div()
                    .id(("nav", i))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        match i {
                            0 => this.view = View::Home,
                            1 => this.view = View::Library,
                            _ => return,
                        }
                        cx.notify();
                    }))
                    .h(px(36.0))
                    .px(px(12.0))
                    .rounded(px(8.0))
                    .flex()
                    .items_center()
                    .text_size(px(14.0))
                    .when(active, |d| d.bg(rgb(t.raised)).text_color(rgb(t.accent)))
                    .when(!active, |d| d.text_color(rgb(t.text)))
                    .child(*n)
            }));

        let header = div()
            .px(px(PAD_X))
            .pt(px(20.0))
            .pb(px(16.0))
            .flex()
            .flex_col()
            .gap(px(16.0))
            .child(div().w(px(520.0)).child(Input::new(&self.search)))
            .when(matches!(self.view, View::Library) && self.search_query.is_empty(), |d| d.child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(12.0))
                    .child(
                        div()
                            .text_size(px(34.0))
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(rgb(t.text_strong))
                            .child("Albums"),
                    )
                    .child(
                        div()
                            .text_size(px(13.0))
                            .text_color(rgb(t.muted))
                            .child(format!(
                                "{} albums · {}",
                                self.cells(), self.status
                            )),
                    ),
            ));

        let grid = uniform_list(
            "albums",
            rows,
            cx.processor(|this, range: std::ops::Range<usize>, _window, cx| {
                range.map(|r| this.row(r, cx)).collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.scroll)
        .flex_1();

        let player = self.player_bar(&t, cx);

        let el = div()
            .key_context("Aulos")
            .track_focus(&self.focus)
            .capture_key_down(|ev: &gpui::KeyDownEvent, window, cx| {
                if std::env::var_os("AULOS_KEYLOG").is_some() {
                    eprintln!("[keys] {:?} has_focus={}", ev.keystroke, window.focused(cx).is_some());
                }
            })
            .on_action(cx.listener(|this, _: &PlayPause, _, cx| this.transport(Transport::PlayPause, cx)))
            .on_action(cx.listener(|this, _: &NextTrack, _, cx| this.transport(Transport::Next, cx)))
            .on_action(cx.listener(|this, _: &PrevTrack, _, cx| this.transport(Transport::Prev, cx)))
            .on_action(cx.listener(|this, _: &SeekForward, _, cx| this.transport(Transport::Seek(10_000), cx)))
            .on_action(cx.listener(|this, _: &SeekBack, _, cx| this.transport(Transport::Seek(-10_000), cx)))
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                use gpui::Focusable;
                let h = this.search.focus_handle(cx);
                window.focus(&h, cx);
            }))
            .on_action(cx.listener(|this, _: &Back, _, cx| {
                this.view = View::Library;
                cx.notify();
            }))
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(t.base))
            .text_color(rgb(t.text))
            .font_family("Inter")
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(sidebar)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .flex()
                            .flex_col()
                            .child(header)
                            .child(match &self.view {
                                _ if !self.search_query.is_empty() => self.search_view(&t, cx),
                                View::Home => self.home_view(&t, cx),
                                View::Library => grid.into_any_element(),
                                View::Album { ix, tracks } => {
                                    let (ix, tracks) = (*ix, tracks.clone());
                                    self.album_view(ix, tracks.as_ref(), &t, cx)
                                }
                            }),
                    ),
            )
            .child(player);
        self.last_render_us = t0.elapsed().as_micros();
        el
    }
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let base = std::env::var("PLEX_URL").unwrap_or_else(|_| "http://plex-server:32400".into());
    let token = std::env::var("PLEX_TOKEN").expect("PLEX_TOKEN not set; set it in the environment");
    let section = std::env::var("PLEX_SECTION").unwrap_or_else(|_| "5".into());

    // tokio lives on its own threads; GPUI never sees it except via channels.
    let (art_tx, art_rx_req) = mpsc::unbounded_channel::<ArtRequest>();
    let (art_res_tx, art_rx) = mpsc::unbounded_channel::<ArtResult>();
    let (albums_tx, albums_rx) = mpsc::unbounded_channel();
    let (tracks_tx, tracks_rx) = mpsc::unbounded_channel::<TracksRequest>();
    let plex_creds = (base.clone(), token.clone());
    let (transport_tx, transport_rx) = mpsc::unbounded_channel::<Transport>();
    let (search_tx, search_rx) = mpsc::unbounded_channel::<SearchRequest>();
    let (hubs_tx, hubs_rx) = mpsc::unbounded_channel::<HubsRequest>();
    let mpris_tx = mpris::start(transport_tx);
    let client = plex::Client::new(base, token);
    std::thread::Builder::new()
        .name("tokio".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(plex::run(client, section, albums_tx, art_rx_req, art_res_tx, tracks_rx, search_rx, hubs_rx));
        })
        .unwrap();

    gpui_platform::application().run(move |cx: &mut App| {
        gpui_component::init(cx);
        cx.bind_keys([
            gpui::KeyBinding::new("space", PlayPause, Some("Aulos && !Input")),
            gpui::KeyBinding::new("n", NextTrack, Some("Aulos && !Input")),
            gpui::KeyBinding::new("p", PrevTrack, Some("Aulos && !Input")),
            gpui::KeyBinding::new("right", SeekForward, Some("Aulos && !Input")),
            gpui::KeyBinding::new("left", SeekBack, Some("Aulos && !Input")),
            gpui::KeyBinding::new("escape", Back, Some("Aulos && !Input")),
            gpui::KeyBinding::new("ctrl-k", FocusSearch, Some("Aulos")),
        ]);
        let bounds = Bounds::centered(None, size(px(1440.0), px(900.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                app_id: Some("aulos-spike".into()),
                ..Default::default()
            },
            move |window, cx| {
                let view = cx.new(|cx| Aulos::new(window, cx, art_tx, art_rx, albums_rx, tracks_tx, plex_creds, transport_rx, mpris_tx, search_tx, hubs_tx));
                cx.new(|cx| gpui_component::Root::new(view, window, cx))
            },
        )
        .unwrap();
        cx.activate(true);
    });
}

#[allow(dead_code)]
fn _unused(_: Pixels) {}
