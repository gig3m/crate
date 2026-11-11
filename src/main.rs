//! Aulos GPUI spike: themed shell, virtualized album grid over a real Plex
//! library, cover art fetched on a tokio side thread, and a scroll benchmark.
//!
//! Env:
//!   PLEX_URL      (default http://plex-server:32400)
//!   PLEX_TOKEN    (required; set in the environment)
//!   PLEX_SECTION  (default 5)
//!   AULOS_ITEMS   (default 50000; real albums are cycled to fill)
//!   AULOS_BENCH   (set: auto-scroll, print frame stats, quit)

mod album;
mod artist;
mod home;
mod icons;
mod library;
mod pages;
mod palette;
mod mpris;
mod now_playing;
mod tile;
mod player_bar;
mod settings;
mod shell;
mod stations;
mod plex;
mod theme;

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{
    App, Bounds, Context, Entity, Image, ImageFormat, Pixels, Point, SharedString,
    Window, WindowBounds, WindowOptions, div, prelude::*, px, rgb, size, uniform_list,
    UniformListScrollHandle,
};
use gpui_component::input::InputState;
use tokio::sync::mpsc;

use aulos_spike::audio::{Engine, Event as AudioEvent, Source};
use mpris::{Transport, Update as MprisUpdate};
use plex::{Album, ArtRequest, ArtResult, ArtistInfo, ArtistRequest, HubsRequest, SearchHit, SearchRequest, Track, TracksRequest};

gpui::actions!(aulos, [PlayPause, NextTrack, PrevTrack, SeekForward, SeekBack, Back, FocusSearch, ToggleNowPlaying, PaletteRadio]);

#[derive(Clone)]
enum View {
    Home,
    Library,
    Album { ix: usize, tracks: Option<Vec<Track>> },
    Artist { key: String, name: String, info: Option<ArtistInfo> },
    /// A canvas page not built yet.
    Soon(&'static str),
}
use theme::Theme;

pub(crate) const SIDEBAR_W: f32 = 240.0;
pub(crate) const PAD_X: f32 = 36.0;

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
    home: Option<plex::Home>,
    /// Back/forward history.
    back: Vec<View>,
    fwd: Vec<View>,
    artist_tx: mpsc::UnboundedSender<ArtistRequest>,
    mpris: tokio::sync::mpsc::UnboundedSender<MprisUpdate>,
    /// Track rating key -> details (levels for the seek bar, lyrics, file...).
    /// None while loading.
    track_detail: HashMap<String, Option<plex::TrackDetail>>,
    /// Width of the main content column, set each render.
    content_w: f32,
    /// Bumped per queue started (Now::queue_id) and per play request, so late
    /// async replies can tell they've been superseded.
    queue_seq: u64,
    play_intent: u64,
    /// Album grid order for the current sort/filters (None: the cached list as-is).
    lib_view: Option<Vec<usize>>,
    lib_filters: Vec<library::LibFilter>,
    lib_sort: library::LibSort,
    lib_gen: u64,
    card_w: f32,
    /// Command palette (Ctrl K) open, selected row, scope chip.
    palette: bool,
    pal_sel: usize,
    pal_scope: usize,
    test_search: Option<String>,
    /// Full-window Now Playing, and its panel tab (queue / signal / info).
    now_playing: bool,
    np_tab: usize,
    /// Compact layout: showing the Playing tab (vs. a page).
    compact_playing: bool,
    /// Server sonic analysis progress, for the Stations page.
    sonic_progress: Option<u32>,
    audio_info: Option<settings::AudioInfo>,
    prompt_copied: bool,
    history: Option<Vec<plex::HistoryItem>>,
    top_tracks: Option<Vec<(Track, u64)>>,
    /// Artists derived from the album list: (key, name, album count).
    artist_cache: Vec<(String, String, usize)>,
    artist_cache_for: usize,
    artist_scroll: UniformListScrollHandle,
    /// Album rating key -> details (None while loading).
    album_detail: HashMap<String, Option<plex::AlbumDetail>>,
    focus: gpui::FocusHandle,
}

struct Now {
    /// What started playback: an album title or station name.
    context: String,
    /// Album rating key when an album is the context.
    context_key: Option<String>,
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
    /// Station key to fetch more tracks from as the queue runs low (radio).
    refill: Option<String>,
    refilling: bool,
    /// One past the last queue index the current engine was given.
    engine_end: usize,
    /// Identifies this queue so late replies (radio refills) can't land on another.
    queue_id: u64,
    /// The user stopped, or the engine failed: don't continue on our own.
    stopped: bool,
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
        artist_tx: mpsc::UnboundedSender<ArtistRequest>,
    ) -> Self {
        {
            let (reply, rx) = tokio::sync::oneshot::channel();
            let _ = hubs_tx.send(HubsRequest { reply });
            cx.spawn(async move |this, cx| match rx.await {
                Ok(Ok(home)) => {
                    this.update(cx, |this, cx| {
                        let thumbs = home.rows.iter().flat_map(|r| r.items.iter().map(|i| i.thumb.clone()));
                        let thumbs: Vec<String> = thumbs
                            .chain(home.playlists.iter().take(8).map(|p| p.thumb.clone()))
                            .chain(home.last_played.iter().map(|(a, _)| a.thumb.clone()))
                            .collect();
                        for th in thumbs {
                            if !th.is_empty() && this.requested.insert(th.clone()) {
                                let _ = this.art_tx.send(ArtRequest { thumb: th });
                            }
                        }
                        // Test hook: start the Nth station.
                        if let Some(n) = std::env::var("AULOS_STATION").ok().and_then(|v| v.parse::<usize>().ok())
                            && let Some(st) = home.rows.iter().filter(|r| r.kind == "station").flat_map(|r| r.items.iter()).nth(n)
                        {
                            this.play_station(st.key.clone(), st.title.clone(), cx);
                        }
                        if let Some(n) = std::env::var("AULOS_PLAYLIST").ok().and_then(|v| v.parse::<usize>().ok())
                            && let Some(p) = home.playlists.get(n)
                        {
                            this.play_playlist(p.rating_key.clone(), p.title.clone(), cx);
                        }
                        this.home = Some(home);
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
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search, play, or run a command"));
        cx.subscribe(&search, |this, input, ev: &gpui_component::input::InputEvent, cx| {
            use gpui_component::input::InputEvent;
            if let InputEvent::Change = ev {
                let q = input.read(cx).value().to_string();
                this.pal_sel = 0;
                this.on_search_change(q, cx);
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
                        // Album indices (open page, history) point into the old list:
                        // remap them by rating key, dropping albums that vanished.
                        let key_of = |v: &View, albums: &[Album]| match v {
                            View::Album { ix, .. } => albums.get(*ix).map(|a| a.rating_key.clone()),
                            _ => None,
                        };
                        let old: Vec<Option<String>> =
                            std::iter::once(&this.view).chain(this.back.iter()).chain(this.fwd.iter()).map(|v| key_of(v, &this.albums)).collect();
                        this.albums = albums;
                        let pos: HashMap<&str, usize> = this.albums.iter().enumerate().map(|(i, a)| (a.rating_key.as_str(), i)).collect();
                        let remap = |v: &mut View, key: &Option<String>| -> bool {
                            match (v, key) {
                                (View::Album { ix, .. }, Some(k)) => match pos.get(k.as_str()) {
                                    Some(&n) => {
                                        *ix = n;
                                        true
                                    }
                                    None => false,
                                },
                                (View::Album { .. }, None) => false,
                                _ => true,
                            }
                        };
                        let mut keys = old.into_iter();
                        let cur_key = keys.next().flatten();
                        if !remap(&mut this.view, &cur_key) {
                            this.view = View::Library;
                        }
                        let back: Vec<View> = std::mem::take(&mut this.back);
                        this.back = back.into_iter().filter_map(|mut v| remap(&mut v, &keys.next().flatten()).then_some(v)).collect();
                        let fwd: Vec<View> = std::mem::take(&mut this.fwd);
                        this.fwd = fwd.into_iter().filter_map(|mut v| remap(&mut v, &keys.next().flatten()).then_some(v)).collect();
                        // A sorted/filtered grid holds indices too.
                        if this.lib_view.is_some() {
                            this.refresh_library(cx);
                        }
                        if !std::mem::replace(&mut first, false) {
                            cx.notify();
                            return;
                        }
                        // Test hooks: AULOS_SORT=artist|title|year|plays, AULOS_FILTER=Decade:1990s:decade=1990
                        if let Ok(v) = std::env::var("AULOS_SORT") {
                            this.lib_sort = library::LibSort::ALL
                                .into_iter()
                                .find(|s| s.label().to_lowercase().starts_with(&v))
                                .unwrap_or(library::LibSort::Added);
                            this.go(View::Library);
                            this.refresh_library(cx);
                        }
                        if let Ok(v) = std::env::var("AULOS_FILTER") {
                            let p: Vec<&str> = v.splitn(3, ':').collect();
                            if let [kind, label, query] = p[..] {
                                let kind: &'static str = Box::leak(kind.to_string().into_boxed_str());
                                this.add_filter(library::LibFilter { kind, label: label.into(), query: query.into() }, cx);
                            }
                        }
                        if std::env::var_os("AULOS_AUDIO_SETTINGS").is_some() {
                            this.open_audio_settings(cx);
                        }
                        if let Ok(page) = std::env::var("AULOS_PAGE") {
                            let page: &'static str = Box::leak(page.into_boxed_str());
                            this.go(View::Soon(page));
                            this.load_page(page, cx);
                        }
                        if std::env::var_os("AULOS_STATIONS").is_some() {
                            this.go(View::Soon("stations"));
                            this.refresh_sonic_progress(cx);
                        }
                        if let Ok(q) = std::env::var("AULOS_QUERY_RADIO") {
                            this.play_station(format!("query:{q}"), "Test radio".into(), cx);
                        }
                        if let Ok(key) = std::env::var("AULOS_RADIO") {
                            this.play_radio(&key, "Radio".into(), cx);
                        }
                        if let Some(ix) = this.autoplay.take() {
                            this.play_album(ix, cx);
                        } else if let Ok(key) = std::env::var("AULOS_ARTIST_KEY") {
                                this.open_artist(key, std::env::var("AULOS_ARTIST_NAME").unwrap_or_default(), cx);
                        } else {
                            let find = |var: &str| {
                                let key = std::env::var(var).ok()?;
                                this.albums.iter().position(|a| a.rating_key == key)
                            };
                            let (open, play) = (find("AULOS_OPEN_KEY"), find("AULOS_AUTOPLAY_KEY"));
                            if let Some(ix) = open {
                                this.open_album(ix, cx);
                            }
                            if let Some(ix) = play {
                                this.play_album(ix, cx);
                            }
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
            back: Vec::new(),
            track_detail: HashMap::new(),
            content_w: 1000.0,
            queue_seq: 0,
            play_intent: 0,
            lib_view: None,
            lib_filters: Vec::new(),
            lib_sort: library::LibSort::Added,
            lib_gen: 0,
            card_w: 140.0,
            palette: false,
            pal_sel: 0,
            pal_scope: 0,
            test_search: std::env::var("AULOS_SEARCH").ok(),
            now_playing: std::env::var_os("AULOS_NOW_PLAYING").is_some(),
            sonic_progress: None,
            compact_playing: true,
            audio_info: None,
            prompt_copied: false,
            history: None,
            top_tracks: None,
            artist_cache: Vec::new(),
            artist_cache_for: usize::MAX,
            artist_scroll: UniformListScrollHandle::new(),
            np_tab: std::env::var("AULOS_NP_TAB").ok().and_then(|v| v.parse().ok()).unwrap_or(0),
            album_detail: HashMap::new(),
            fwd: Vec::new(),
            album_scroll: gpui::ScrollHandle::new(),
            search_tx,
            search_gen: 0,
            search_query: String::new(),
            hits: Vec::new(),
            reset_search: false,
            home: None,
            artist_tx,
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
                self.open_artist(h.rating_key.clone(), h.title.clone(), cx);
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
        self.request_thumb(&album.thumb);
    }

    fn request_thumb(&mut self, thumb: &str) {
        if !thumb.is_empty() && !self.art.contains_key(thumb) && self.requested.insert(thumb.to_string()) {
            let _ = self.art_tx.send(ArtRequest { thumb: thumb.to_string() });
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

    fn open_artist(&mut self, key: String, name: String, cx: &mut Context<Self>) {
        if key.is_empty() {
            return;
        }
        let mine: Vec<Album> = self.albums.iter().filter(|a| a.artist_key == key).cloned().collect();
        for a in &mine {
            self.request_art(a);
        }
        self.go(View::Artist { key: key.clone(), name, info: None });
        self.album_scroll = gpui::ScrollHandle::new();
        cx.notify();
        self.load_artist_info(key, cx);
    }

    fn load_artist_info(&mut self, key: String, cx: &mut Context<Self>) {
        let (reply, rx) = tokio::sync::oneshot::channel();
        let _ = self.artist_tx.send(ArtistRequest { key: key.clone(), reply });
        cx.spawn(async move |this, cx| match rx.await {
            Ok(Ok(info)) => {
                this.update(cx, |this, cx| {
                    this.request_artist_art(&info);
                    if let View::Artist { key: k, info: slot, .. } = &mut this.view
                        && *k == key
                    {
                        *slot = Some(info);
                        cx.notify();
                    }
                })
                .ok();
            }
            Ok(Err(e)) => eprintln!("[aulos] artist: {e:#}"),
            Err(_) => {}
        })
        .detach();
    }

    /// Play `track_key` in the context of its album.
    fn play_track_in_album(&mut self, album_key: &str, track_key: String, cx: &mut Context<Self>) {
        let Some(album) = self.albums.iter().find(|a| a.rating_key == album_key).cloned() else { return };
        self.request_art(&album);
        self.with_tracks(album, cx, move |this, album, tracks, cx| {
            let start = tracks.iter().position(|t| t.rating_key == track_key).unwrap_or(0);
            this.play_tracks(album, tracks, start, cx);
        });
    }

    /// Navigate, recording history.
    fn go(&mut self, v: View) {
        let old = std::mem::replace(&mut self.view, v);
        self.compact_playing = false;
        self.back.push(old);
        if self.back.len() > 50 {
            self.back.remove(0);
        }
        self.fwd.clear();
        self.album_scroll = gpui::ScrollHandle::new();
    }

    fn go_back(&mut self, cx: &mut Context<Self>) {
        if let Some(v) = self.back.pop() {
            let cur = std::mem::replace(&mut self.view, v);
            self.fwd.push(cur);
            self.album_scroll = gpui::ScrollHandle::new();
            self.reload_view(cx);
            cx.notify();
        }
    }

    /// A view restored from history may still be waiting on data whose reply
    /// arrived after we'd left it; ask again.
    fn reload_view(&mut self, cx: &mut Context<Self>) {
        match &self.view {
            View::Album { ix, tracks: None } => {
                let ix = *ix;
                self.load_album_tracks(ix, cx);
            }
            View::Artist { key, info: None, .. } => {
                let key = key.clone();
                self.load_artist_info(key, cx);
            }
            _ => {}
        }
    }

    fn go_forward(&mut self, cx: &mut Context<Self>) {
        if let Some(v) = self.fwd.pop() {
            let cur = std::mem::replace(&mut self.view, v);
            self.back.push(cur);
            self.album_scroll = gpui::ScrollHandle::new();
            self.reload_view(cx);
            cx.notify();
        }
    }

    fn open_album(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(album) = self.album(ix).cloned() else { return };
        self.request_art(&album);
        self.go(View::Album { ix, tracks: None });
        self.request_album_detail(album.rating_key.clone(), cx);
        self.album_scroll = gpui::ScrollHandle::new();
        cx.notify();
        self.load_album_tracks(ix, cx);
    }

    fn load_album_tracks(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(album) = self.album(ix).cloned() else { return };
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
        self.start_queue(album.title, Some(album.rating_key), tracks, start, cx);
    }

    /// Play `tracks` from `start`; `context` names what's playing.
    fn start_queue(&mut self, context: String, context_key: Option<String>, tracks: Vec<Track>, start: usize, cx: &mut Context<Self>) {
        // Every index into `now.tracks` assumes idx < len.
        if start >= tracks.len() {
            self.status = format!("{context}: nothing to play").into();
            cx.notify();
            return;
        }
        self.queue_seq += 1;
        self.play_intent += 1;
        for t in &tracks {
            self.request_thumb(&t.thumb.clone());
        }
        self.now = Some(Now {
            context,
            context_key,
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
            refill: None,
            refilling: false,
            engine_end: 0,
            queue_id: self.queue_seq,
            stopped: false,
        });
        self.play_from(start, 0, cx);
    }

    fn art_path(album_key: &str) -> std::path::PathBuf {
        std::path::PathBuf::from(format!("/scratch/aulos-cache/art-{album_key}.jpg"))
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
                now.stopped = false;
                now.engine_end = now.tracks.len();
                self.status = format!("Playing {}", now.context).into();
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
                now.stopped = true;
                now.paused = true;
                let _ = self.mpris.send(MprisUpdate::Status { playing: false, stopped: true });
                cx.notify();
            }
        }
    }

    /// Returns false once playback is over.
    fn poll_engine(&mut self, cx: &mut Context<Self>) -> bool {
        let (Some(engine), Some(now)) = (self.engine.as_ref(), self.now.as_mut()) else { return false };
        let mut failed = false;
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
                    self.status = format!("Audio error: {e}").into();
                    now.finished = true;
                    now.stopped = true;
                    failed = true;
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
        now.idx = (now.start_idx + rel).min(now.tracks.len().saturating_sub(1));
        now.elapsed_ms = (played_us - base.min(played_us)) / 1000 + if rel == 0 { now.start_ms } else { 0 };

        // Cover arrived after the track was announced: write it and re-announce.
        let cur = &now.tracks[now.idx];
        let art = Self::art_path(&cur.album_key);
        if !art.exists()
            && let Some(img) = self.art.get(&cur.thumb)
                && std::fs::write(&art, img.bytes()).is_ok() {
                    now.sent_idx = None;
                }
        if now.sent_idx != Some(now.idx) {
            now.sent_idx = Some(now.idx);
            let tr = &now.tracks[now.idx];
            let art = Self::art_path(&tr.album_key);
            let _ = self.mpris.send(MprisUpdate::Track {
                id: tr.part_key.split('/').nth(3).unwrap_or("0").to_string(),
                title: tr.title.clone(),
                artist: tr.artist.clone(),
                album: tr.album_title.clone(),
                length_ms: tr.duration_ms,
                art_url: art.exists().then(|| format!("file://{}", art.display())),
            });
        }
        let rk = now.tracks[now.idx].rating_key.clone();
        let album_key = now.tracks[now.idx].album_key.clone();
        self.request_track_detail(rk, cx);
        // Now Playing's ambient field comes from the album's UltraBlur colors.
        self.request_album_detail(album_key, cx);
        let Some(now) = self.now.as_mut() else { return false };
        // Radio: fetch the next window when two tracks from the end.
        if let Some(key) = now.refill.clone()
            && !now.refilling
            && now.idx + 2 >= now.tracks.len()
        {
            now.refilling = true;
            let qid = now.queue_id;
            let rx = fetch_more(key);
            cx.spawn(async move |this, cx| {
                let res = rx.await;
                this.update(cx, |this, cx| {
                    // Another queue started meanwhile: these tracks aren't for it.
                    let Some(now) = this.now.as_mut().filter(|n| n.queue_id == qid) else { return };
                    now.refilling = false;
                    match res {
                        Ok(Ok(more)) => {
                            let have: HashSet<String> = now.tracks.iter().map(|t| t.rating_key.clone()).collect();
                            let more: Vec<Track> = more.into_iter().filter(|t| !have.contains(&t.rating_key)).collect();
                            if more.is_empty() {
                                now.refill = None; // station ran dry
                            }
                            let thumbs: Vec<String> = more.iter().map(|t| t.thumb.clone()).collect();
                            now.tracks.extend(more);
                            for th in thumbs {
                                this.request_thumb(&th);
                            }
                            // The engine already ran out while we fetched: continue now.
                            if let Some(n) = this.now.as_ref()
                                && n.finished
                                && !n.stopped
                                && n.engine_end < n.tracks.len()
                            {
                                let from = n.engine_end;
                                this.play_from(from, 0, cx);
                            }
                            cx.notify();
                        }
                        Ok(Err(e)) => {
                            // Don't retry on every poll; the queue ends where it is.
                            eprintln!("[aulos] refill: {e:#}");
                            now.refill = None;
                        }
                        Err(_) => {}
                    }
                })
                .ok();
            })
            .detach();
        }
        // The engine reached the end of its queue but the queue has grown.
        if now.finished && !failed && !now.stopped && now.engine_end < now.tracks.len() {
            let from = now.engine_end;
            self.play_from(from, 0, cx);
            return false;
        }
        let _ = self.mpris.send(MprisUpdate::Position(now.elapsed_ms));
        if now.finished {
            let _ = self.mpris.send(MprisUpdate::Status { playing: false, stopped: true });
        }
        cx.notify();
        let keep = !now.finished;
        if failed {
            // Stop the pipeline; Drop stops both engine threads.
            self.engine = None;
        }
        keep
    }

    /// Grid cells: the real library, or AULOS_ITEMS for the scroll benchmark.
    fn request_track_detail(&mut self, rk: String, cx: &mut Context<Self>) {
        if self.track_detail.contains_key(&rk) {
            return;
        }
        self.track_detail.insert(rk.clone(), None);
        let rx = plex_call(move |c| async move { c.track_detail(&rk, player_bar::BARS).await.map(|d| (rk, d)) });
        cx.spawn(async move |this, cx| match rx.await {
            Ok(Ok((rk, d))) => {
                this.update(cx, |this, cx| {
                    this.track_detail.insert(rk, Some(d));
                    cx.notify();
                })
                .ok();
            }
            Ok(Err(e)) => eprintln!("[aulos] track detail: {e:#}"),
            Err(_) => {}
        })
        .detach();
    }

    fn cells(&self) -> usize {
        match &self.lib_view {
            Some(ix) => ix.len(),
            None if self.total == 0 => self.albums.len(),
            None => self.total,
        }
    }

    /// Grid cell -> album index.
    fn cell_album(&self, cell: usize) -> usize {
        match &self.lib_view {
            Some(ix) => ix.get(cell).copied().unwrap_or(0),
            None => cell,
        }
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
        let row_h = self.row_h();
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
        let max_y = (cells.div_ceil(self.cols.max(1)) as f32) * row_h;
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

    fn open_hub_item(&mut self, kind: &str, key: &str, title: &str, cx: &mut Context<Self>) {
        match kind {
            "album" => {
                if let Some(ix) = self.albums.iter().position(|a| a.rating_key == key) {
                    self.open_album(ix, cx);
                }
            }
            "artist" => self.open_artist(key.to_string(), title.to_string(), cx),
            _ => eprintln!("[aulos] station {title:?} not implemented yet"),
        }
    }


}

/// Fisher–Yates with a time-seeded xorshift (no rand dependency).
fn shuffle<T>(v: &mut [T]) {
    let mut x = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1) | 1;
    for i in (1..v.len()).rev() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        v.swap(i, (x % (i as u64 + 1)) as usize);
    }
}

fn fmt_ms(ms: u64) -> String {
    format!("{}:{:02}", ms / 60_000, (ms / 1000) % 60)
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
        // Test hook: open the palette with a query once the library is in.
        if !self.albums.is_empty()
            && let Some(q) = self.test_search.take()
        {
            self.search.update(cx, |s, cx| s.set_value(q.clone(), window, cx));
            self.open_palette(window, cx);
            self.on_search_change(q, cx);
        }
        if std::mem::take(&mut self.reset_search) {
            self.search.update(cx, |s, cx| s.set_value("", window, cx));
            self.search_query.clear();
            window.focus(&self.focus, cx);
        }
        let vw: f32 = window.viewport_size().width.into();
        // Narrow tiled window: the canvas Tile layout (no sidebar, bottom nav).
        let compact = vw < 720.0;
        let side = if compact { 0.0 } else { SIDEBAR_W };
        let usable = (vw - side - PAD_X * 2.0).max(240.0);
        self.content_w = usable;
        self.grid_layout(vw - side - PAD_X);
        let rows = self.cells().div_ceil(self.cols);

        if self.bench.is_some() {
            self.bench_tick(window, cx);
        }

        let sidebar = (!compact).then(|| self.sidebar(&t, cx));

        let header = matches!(self.view, View::Library).then(|| self.library_header(&t, cx));
        let rail = self.letter_rail(&t, cx);

        let grid = uniform_list(
            "albums",
            rows,
            cx.processor(|this, range: std::ops::Range<usize>, _window, cx| {
                range.map(|r| this.row(r, cx)).collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.scroll)
        .flex_1();
        let grid = div().flex_1().min_h_0().flex().child(grid).children(rail);

        let page = match &self.view {
            View::Home => self.home_view(&t, cx),
            View::Artist { key, name, info } => {
                let (key, name, info) = (key.clone(), name.clone(), info.clone());
                self.artist_view(&key, &name, info.as_ref(), &t, cx)
            }
            View::Library => grid.into_any_element(),
            View::Soon("stations") => self.stations_view(&t, cx),
            View::Soon("audio") => self.audio_settings_view(&t, cx),
            View::Soon("artists") => self.artists_view(&t, cx),
            View::Soon("tracks") => self.tracks_view(&t, cx),
            View::Soon("history") => self.history_view(&t, cx),
            View::Soon("playlists") => self.playlists_view(&t, cx),
            View::Soon("genres") => self.genres_view(&t, cx),
            View::Soon(name) => self.soon_view(name, &t),
            View::Album { ix, tracks } => {
                let (ix, tracks) = (*ix, tracks.clone());
                self.album_view(ix, tracks.as_ref(), &t, cx)
            }
        };
        let main_area = if compact && self.compact_playing && self.now.is_some() {
            self.tile_view(&t, cx)
        } else {
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .children(sidebar)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .flex()
                        .flex_col()
                        .when(!compact, |d| d.child(self.top_bar(&t, cx)))
                        .when(compact, |d| d.pt(px(16.0)))
                        .children(header)
                        .child(page),
                )
                .into_any_element()
        };
        let player = if compact { self.compact_nav(&t, cx) } else { self.player_bar(&t, cx) };
        let palette = self.palette.then(|| self.palette_view(&t, cx));
        let now_playing = (self.now_playing && self.now.is_some() && !compact)
            .then(|| div().absolute().inset_0().child(self.now_playing_view(&t, cx)));

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
                this.open_palette(window, cx);
            }))
            .on_action(cx.listener(|this, _: &Back, _, cx| {
                if this.now_playing {
                    this.now_playing = false;
                    cx.notify();
                } else {
                    this.go_back(cx)
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleNowPlaying, _, cx| {
                this.now_playing = !this.now_playing && this.now.is_some();
                cx.notify();
            }))
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(rgb(t.base))
            .text_color(rgb(t.text))
            .font_family("Inter")
            .child(main_area)
            .child(player)
            .children(now_playing)
            .children(palette);
        self.last_render_us = t0.elapsed().as_micros();
        el
    }
}

/// The tokio runtime and Plex client, for one-off requests from the UI.
static PLEX: std::sync::OnceLock<(tokio::runtime::Handle, plex::Client)> = std::sync::OnceLock::new();

/// Run a Plex request on the tokio side; await the receiver from `cx.spawn`.
fn plex_call<T, F>(f: impl FnOnce(plex::Client) -> F) -> tokio::sync::oneshot::Receiver<anyhow::Result<T>>
where
    T: Send + 'static,
    F: std::future::Future<Output = anyhow::Result<T>> + Send + 'static,
{
    let (tx, rx) = tokio::sync::oneshot::channel();
    let (handle, client) = PLEX.get().expect("plex runtime");
    let fut = f(client.clone());
    handle.spawn(async move {
        let _ = tx.send(fut.await);
    });
    rx
}

/// More tracks for a radio: a Plex station/radio key, or `query:<filter>`
/// for random tracks matching a library filter.
fn fetch_more(key: String) -> tokio::sync::oneshot::Receiver<anyhow::Result<Vec<Track>>> {
    plex_call(move |c| async move {
        match key.strip_prefix("query:") {
            Some(filter) => {
                let section = std::env::var("PLEX_SECTION").unwrap_or_else(|_| "5".into());
                c.random_tracks(&section, filter, 100).await
            }
            None => c.play_queue(&key).await.map(|(_, t)| t),
        }
    })
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info,zbus=warn")).init();
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
    let (artist_tx, artist_rx) = mpsc::unbounded_channel::<ArtistRequest>();
    let mpris_tx = mpris::start(transport_tx);
    let client = plex::Client::new(base, token);
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let _ = PLEX.set((rt.handle().clone(), client.clone()));
    std::thread::Builder::new()
        .name("tokio".into())
        .spawn(move || {
            rt.block_on(plex::run(client, section, albums_tx, art_rx_req, art_res_tx, tracks_rx, search_rx, hubs_rx, artist_rx));
        })
        .unwrap();

    gpui_platform::application().with_assets(icons::Icons).run(move |cx: &mut App| {
        gpui_component::init(cx);
        // Static Inter weights: GPUI on Linux renders a variable font at one weight.
        cx.text_system()
            .add_fonts(vec![
                std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/Inter-Regular.ttf").as_slice()),
                std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/Inter-Medium.ttf").as_slice()),
                std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/Inter-SemiBold.ttf").as_slice()),
                std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/Inter-Bold.ttf").as_slice()),
            ])
            .expect("bundled fonts");
        cx.bind_keys([
            gpui::KeyBinding::new("space", PlayPause, Some("Aulos && !Input")),
            gpui::KeyBinding::new("n", NextTrack, Some("Aulos && !Input")),
            gpui::KeyBinding::new("p", PrevTrack, Some("Aulos && !Input")),
            gpui::KeyBinding::new("right", SeekForward, Some("Aulos && !Input")),
            gpui::KeyBinding::new("left", SeekBack, Some("Aulos && !Input")),
            gpui::KeyBinding::new("escape", Back, Some("Aulos && !Input")),
            gpui::KeyBinding::new("f", ToggleNowPlaying, Some("Aulos && !Input")),
            // gpui-component binds tab to IndentInline in its Input; bound later at
            // the same depth, this wins inside the palette's search box.
            gpui::KeyBinding::new("tab", PaletteRadio, Some("Palette > Input")),
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
                let view = cx.new(|cx| Aulos::new(window, cx, art_tx, art_rx, albums_rx, tracks_tx, plex_creds, transport_rx, mpris_tx, search_tx, hubs_tx, artist_tx));
                cx.new(|cx| gpui_component::Root::new(view, window, cx))
            },
        )
        .unwrap();
        cx.activate(true);
    });
}

#[allow(dead_code)]
fn _unused(_: Pixels) {}
