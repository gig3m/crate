//! Crate GPUI spike: themed shell, virtualized album grid over a real Plex
//! library, cover art fetched on a tokio side thread, and a scroll benchmark.
//!
//! Sign-in: Crate keeps its Plex account in ~/.config/crate/account.toml (0600)
//! and shows a sign-in screen when there's none. Flags: --sign-out,
//! --import-env (save PLEX_URL/PLEX_TOKEN/PLEX_SECTION from the environment).
//! Env (overrides the stored account):
//!   PLEX_URL, PLEX_TOKEN, PLEX_SECTION
//!   CRATE_ITEMS   (default 50000; real albums are cycled to fill)
//!   CRATE_BENCH   (set: auto-scroll, print frame stats, quit)

mod account;
mod album;
mod art;
mod keys;
mod artist;
mod home;
mod icons;
mod instance;
mod library;
mod pages;
mod palette;
mod queue;
mod mpris;
mod now_playing;
mod offline;
mod tile;
mod player_bar;
mod settings;
mod setup;
mod shell;
mod sonic;
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

use crate_audio::audio::{Engine, Event as AudioEvent, Item, Source};
use mpris::{Transport, Update as MprisUpdate};
use plex::{Album, ArtRequest, ArtResult, ArtistInfo, ArtistRequest, HubsRequest, SearchHit, SearchRequest, Track, TracksRequest};

gpui::actions!(crate_app, [PlayPause, NextTrack, PrevTrack, SeekForward, SeekBack, Back, FocusSearch, ToggleNowPlaying, PaletteRadio, Rate1, Rate2, Rate3, Rate4, Rate5, GoHome, GoStations, ShowLyrics, ShowQueue, VolumeUp, VolumeDown, CursorNext, CursorPrev, ActivateCursor, MarkCursor, RadioFromSelection]);

#[derive(Clone)]
// One View at a time (plus the back/forward stacks): the Artist variant's
// size doesn't matter here.
#[allow(clippy::large_enum_variant)]
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

struct Crate {
    theme: Theme,
    theme_gen: String,
    /// Crate's palette (default) or the active Omarchy theme.
    theme_mode: theme::Mode,
    /// Generated station marks and cover placeholders for the current theme.
    marks: std::cell::RefCell<art::Marks>,
    albums: Vec<Album>,
    total: usize,
    status: SharedString,
    /// The status last shown as a toast, and until when it stays up.
    status_seen: SharedString,
    status_until: Option<Instant>,
    /// Loudness leveling setting, and Plex's loudness analysis by track key.
    leveling: theme::Leveling,
    gains: HashMap<String, plex::Gain>,
    /// Ten-band EQ: preset, the custom curve, and one slider per band.
    eq_preset: theme::EqPreset,
    eq_custom: [f32; 10],
    eq_sliders: Vec<Entity<gpui_component::slider::SliderState>>,
    settings_scroll: gpui::ScrollHandle,
    /// Debounces saving the EQ while a slider moves (keyboard steps send no release).
    eq_save_gen: u64,
    /// Sweet fades between albums, in seconds (0 = off).
    fade_secs: u32,
    /// Remote streaming quality, kbps (0 = original file).
    remote_kbps: u32,
    /// A queue waiting on loudness before it starts (its queue_seq); cleared
    /// by any transport action or explicit start, which supersede it.
    pending_start: Option<u64>,
    /// (queue context, note): shown with "Playing …" when that queue starts.
    play_note: Option<(String, String)>,
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
    shuffle: bool,
    repeat: queue::Repeat,
    /// Perceptual volume 0..=1 (the engine gets volume³), and the level to
    /// restore when unmuting.
    volume: f32,
    unmute_to: f32,
    volume_gen: u64,
    /// Accumulated scroll on the volume slider (trackpads send tiny deltas).
    volume_scroll: f32,
    /// Theme previews for Settings, read once when it opens.
    theme_previews: Option<(Theme, Theme)>,
    /// Album grid order for the current sort/filters (None: the cached list as-is).
    lib_view: Option<Vec<usize>>,
    lib_filters: Vec<library::LibFilter>,
    lib_sort: library::LibSort,
    lib_gen: u64,
    card_w: f32,
    /// Albums as a list instead of the grid; multi-selected album keys.
    lib_list: bool,
    selection: Vec<String>,
    /// Command palette (Ctrl K) open, selected row, scope chip.
    palette: bool,
    pal_sel: usize,
    pal_scope: usize,
    /// The search generation that last finished (so "no results" can show).
    search_done: u64,
    test_search: Option<String>,
    /// Keyboard cursor (j/k) in the current list: an Albums cell or a track index.
    cursor: Option<usize>,
    /// Narrow (Tile) layout, from the last render.
    compact: bool,
    /// Test hook: canvas key actions to run once the library is in (CRATE_KEYS="j,j,enter").
    test_keys: Option<String>,
    /// Full-window Now Playing, and its panel tab (queue / signal / info).
    now_playing: bool,
    np_tab: usize,
    /// Compact layout: showing the Playing tab (vs. a page).
    compact_playing: bool,
    /// Server sonic analysis progress, for the Stations page.
    sonic_progress: Option<u32>,
    audio_info: Option<settings::AudioInfo>,
    prompt_copied: bool,
    /// Another launch (or MPRIS Raise) asked for the window.
    raise_requested: bool,
    /// Albums kept offline, and the download in progress.
    offline: offline::Offline,
    /// Sonic analysis results: similar albums / artists per key (None while loading).
    sonic_albums: HashMap<String, Option<Vec<Album>>>,
    /// Card formats of other editions shown on album pages, by album key.
    edition_formats: HashMap<String, (u32, String)>,
    /// Guest DJ (canvas): slips a sonically nearby track in after each one.
    guest_dj: bool,
    /// (queue, track key) the Guest DJ added (badged in that queue), and the
    /// (queue, track key) it has picked after: one pick per track per queue,
    /// however often the track comes round (Prev, repeat, toggling).
    dj_keys: std::collections::HashSet<(u64, String)>,
    dj_done: std::collections::HashSet<(u64, String)>,
    /// Picks that failed while their track played: retryable once playback
    /// moves to another track (not every poll), with the (queue, index) seen last.
    dj_retry: Vec<(u64, String)>,
    dj_seen: Option<(u64, usize)>,
    /// Appears on, by artist key: album keys (fetched after the artist page shows).
    appears_on: HashMap<String, Vec<String>>,
    sonic_artists: HashMap<String, Option<Vec<sonic::ArtistRef>>>,
    sonic: sonic::SonicState,
    /// Artist key -> (singles & EPs, live) album keys; and the open tab.
    artist_kinds: HashMap<String, Option<sonic::AlbumKinds>>,
    /// The discography tab picked per artist (kept across history).
    artist_tabs: HashMap<String, usize>,
    sonic_search: Entity<InputState>,
    history: Option<Vec<plex::HistoryItem>>,
    top_tracks: Option<Vec<(Track, u64)>>,
    /// Artists derived from the album list: (key, name, album count).
    artist_cache: Vec<(String, String, usize)>,
    artist_cache_for: usize,
    artist_scroll: UniformListScrollHandle,
    /// Album rating key -> details (None while loading).
    album_detail: HashMap<String, Option<plex::AlbumDetail>>,
    focus: gpui::FocusHandle,
    /// Closes out the playing track's report when the app quits.
    _on_quit: Option<gpui::Subscription>,
}

struct Now {
    /// What started playback: an album title or station name.
    context: String,
    /// Album rating key when an album is the context.
    context_key: Option<String>,
    tracks: Vec<Track>,
    /// index -> leveling gain the decoder applied, from engine TrackStarted events.
    level: HashMap<usize, f32>,
    /// index -> (source rate, bits), from engine TrackStarted events.
    formats: HashMap<usize, (u32, u32)>,
    /// index -> transcode bitrate (kbps) of what the engine opened; absent = original file.
    transcoded: HashMap<usize, u32>,
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
    /// Play reporting: the track last reported, its state, last known
    /// position, and when we last told the server.
    rep_idx: Option<usize>,
    rep_state: &'static str,
    rep_elapsed: u64,
    rep_at: Instant,
    /// Rating keys in the order the queue started, to undo shuffle.
    orig_order: Vec<String>,
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

impl Crate {
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
                        if let Some(n) = std::env::var("CRATE_STATION").ok().and_then(|v| v.parse::<usize>().ok())
                            && let Some(st) = home.rows.iter().filter(|r| r.kind == "station").flat_map(|r| r.items.iter()).nth(n)
                        {
                            this.play_station(st.key.clone(), st.title.clone(), cx);
                        }
                        if let Some(n) = std::env::var("CRATE_PLAYLIST").ok().and_then(|v| v.parse::<usize>().ok())
                            && let Some(p) = home.playlists.get(n)
                        {
                            this.play_playlist(p.rating_key.clone(), p.title.clone(), cx);
                        }
                        // Recently Added format labels follow in a second pass, so
                        // Home shows without waiting on them.
                        let keys: Vec<String> = home
                            .rows
                            .iter()
                            .filter(|r| r.kind == "album" && r.id.starts_with("music.recent.added"))
                            .flat_map(|r| r.items.iter().map(|i| i.rating_key.clone()))
                            .collect();
                        // Mixes for you: similar artists (subtitle, covers), art, and the
                        // mood mixes' covers in a second pass (Home doesn't wait on them).
                        for (k, _) in Crate::mix_artists(&home) {
                            this.request_sonic_artists(k, cx);
                        }
                        let filters: Vec<String> = home.mixes.iter().map(|m| m.2.clone()).collect();
                        this.home = Some(home);
                        this.request_mix_art();
                        if !filters.is_empty() {
                            let section = crate::section();
                            let rx = plex_call(move |c| async move { c.mix_covers(&section, filters).await });
                            cx.spawn(async move |this, cx| {
                                if let Ok(Ok(covers)) = rx.await {
                                    this.update(cx, |this, cx| {
                                        if let Some(h) = this.home.as_mut() {
                                            for (m, c) in h.mixes.iter_mut().zip(covers) {
                                                m.3 = c;
                                            }
                                        }
                                        this.request_mix_art();
                                        cx.notify();
                                    })
                                    .ok();
                                }
                            })
                            .detach();
                        }
                        if !keys.is_empty() {
                            let rx = plex_call(move |c| async move { c.album_formats(keys).await });
                            cx.spawn(async move |this, cx| {
                                if let Ok(Ok(found)) = rx.await {
                                    this.update(cx, |this, cx| {
                                        let Some(home) = this.home.as_mut() else { return };
                                        let rows = home.rows.iter_mut().filter(|r| r.kind == "album" && r.id.starts_with("music.recent.added"));
                                        for it in rows.flat_map(|r| r.items.iter_mut()) {
                                            if let Some((_, f)) = found.iter().find(|(k, _)| *k == it.rating_key) {
                                                it.format = f.clone();
                                            }
                                        }
                                        cx.notify();
                                    })
                                    .ok();
                                }
                            })
                            .detach();
                        }
                        cx.notify();
                    })
                    .ok();
                }
                Ok(Err(e)) => eprintln!("[crate] hubs: {e:#}"),
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
        let on_quit = cx.on_app_quit(|this: &mut Self, _cx| {
            this.report_stop();
            flush_writes(Duration::from_millis(1500));
            async {}
        });
        // Closing the window drops this entity before app-quit hooks run, so
        // close out the report here, while it still exists.
        let weak = cx.entity().downgrade();
        window.on_window_should_close(cx, move |_, cx| {
            instance::cleanup();
            weak.update(cx, |this, _| this.report_stop()).ok();
            flush_writes(Duration::from_millis(1500));
            true
        });
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let theme_mode = theme::saved_mode();
        let (theme, theme_gen) = Theme::load(theme_mode);
        apply_component_theme(&theme, Some(window), cx);
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search, play, or run a command"));
        let sonic_search = cx.new(|cx| InputState::new(window, cx).placeholder("Search for a track to start or end at"));
        let (eq_preset, eq_custom) = theme::saved_eq();
        let shown = eq_preset.curve().unwrap_or(eq_custom);
        let eq_sliders: Vec<_> = (0..10)
            .map(|i| cx.new(|_| gpui_component::slider::SliderState::new().min(-12.0).max(12.0).step(0.5).default_value(shown[i])))
            .collect();
        for (i, s) in eq_sliders.iter().enumerate() {
            cx.subscribe(s, move |this, _, ev: &gpui_component::slider::SliderEvent, cx| {
                use gpui_component::slider::SliderEvent;
                // Heard while dragging; saved when let go.
                let (v, save) = match ev {
                    SliderEvent::Change(v) => (v, false),
                    SliderEvent::Release(v) => (v, true),
                };
                this.set_eq_band(i, v.start(), save, cx);
            })
            .detach();
        }
        cx.subscribe(&sonic_search, |this, input, ev: &gpui_component::input::InputEvent, cx| {
            if let gpui_component::input::InputEvent::Change = ev {
                let q = input.read(cx).value().to_string();
                this.on_sonic_search(q, cx);
            }
        })
        .detach();
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
                            eprintln!("[crate] {} albums · up to date", albums.len());
                            return;
                        }
                        eprintln!("[crate] {} albums ({from})", albums.len());
                        // Grid cells shift with new albums: the keyboard cursor no longer points where it did.
                        if matches!(this.view, View::Library) {
                            this.cursor = None;
                        }
                        // Album indices (open page, history) point into the old list:
                        // remap them by rating key, dropping albums that vanished.
                        let key_of = |v: &View, albums: &[Album]| match v {
                            View::Album { ix, .. } => albums.get(*ix).map(|a| a.rating_key.clone()),
                            _ => None,
                        };
                        let old: Vec<Option<String>> =
                            std::iter::once(&this.view).chain(this.back.iter()).chain(this.fwd.iter()).map(|v| key_of(v, &this.albums)).collect();
                        this.albums = albums;
                        // Mix collages read album covers: Home may have landed first.
                        this.request_mix_art();
                        // Same for an open artist page's Appears on covers.
                        if let View::Artist { key, .. } = &this.view {
                            let key = key.clone();
                            this.request_appears_art(&key);
                        }
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
                            // A track cursor would be read as a grid cell.
                            this.cursor = None;
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
                        this.pump_offline(cx);
                        // Test hook: keep this album offline.
                        if let Ok(key) = std::env::var("CRATE_KEEP_OFFLINE")
                            && let Some(album) = this.albums.iter().find(|a| a.rating_key == key).cloned()
                        {
                            this.with_tracks(album, cx, |this, album, tracks, cx| this.keep_offline(album, tracks, cx));
                        }
                        // Test hooks: CRATE_SORT=artist|title|year|plays, CRATE_FILTER=Decade:1990s:decade=1990
                        if let Ok(v) = std::env::var("CRATE_SORT") {
                            this.lib_sort = library::LibSort::ALL
                                .into_iter()
                                .find(|s| s.label().to_lowercase().starts_with(&v))
                                .unwrap_or(library::LibSort::Added);
                            this.go(View::Library);
                            this.refresh_library(cx);
                        }
                        if let Ok(v) = std::env::var("CRATE_FILTER") {
                            let p: Vec<&str> = v.splitn(3, ':').collect();
                            if let [kind, label, query] = p[..] {
                                let kind: &'static str = Box::leak(kind.to_string().into_boxed_str());
                                this.add_filter(library::LibFilter { kind, label: label.into(), query: query.into() }, cx);
                            }
                        }
                        if std::env::var_os("CRATE_AUDIO_SETTINGS").is_some() {
                            this.open_audio_settings(cx);
                        }
                        if let Ok(page) = std::env::var("CRATE_PAGE") {
                            let page: &'static str = Box::leak(page.into_boxed_str());
                            this.go(View::Soon(page));
                            this.load_page(page, cx);
                        }
                        // Test hooks: CRATE_SONIC_FROM / CRATE_SONIC_TO (track keys) open
                        // Sonic Adventure with that path; CRATE_SONIC_RADIO plays sonic radio.
                        if let (Ok(f), Ok(t)) = (std::env::var("CRATE_SONIC_FROM"), std::env::var("CRATE_SONIC_TO")) {
                            let mk = |k: String| plex::Track { rating_key: k, title: String::new(), artist: String::new(), duration_ms: 0, part_key: String::new(), album_key: String::new(), album_title: String::new(), thumb: String::new() };
                            this.sonic.from = Some(mk(f));
                            this.sonic.to = Some(mk(t));
                            this.open_sonic(cx);
                            this.compute_sonic_path_pub(cx);
                        }
                        if let Ok(k) = std::env::var("CRATE_SONIC_ALBUM")
                            && let Some(a) = this.albums.iter().find(|a| a.rating_key == k).cloned()
                        {
                            this.play_album_sonic_radio(a, cx);
                        }
                        // Test hooks: CRATE_LIST=1 list view; CRATE_SELECT=key,key preselects.
                        if std::env::var_os("CRATE_LIST").is_some() {
                            this.lib_list = true;
                            this.go(View::Library);
                        }
                        if let Ok(v) = std::env::var("CRATE_SELECT") {
                            this.selection = v.split(',').map(String::from).collect();
                            this.go(View::Library);
                        }
                        if std::env::var_os("CRATE_SELECT_PLAY").is_some() {
                            this.play_selection(false, cx);
                        }
                        if std::env::var_os("CRATE_STATIONS").is_some() {
                            this.go(View::Soon("stations"));
                            this.refresh_sonic_progress(cx);
                        }
                        if let Ok(q) = std::env::var("CRATE_QUERY_RADIO") {
                            this.play_station(format!("query:{q}"), "Test radio".into(), cx);
                        }
                        if let Ok(key) = std::env::var("CRATE_RADIO") {
                            this.play_radio(&key, "Radio".into(), cx);
                        }
                        if let Some(ix) = this.autoplay.take() {
                            this.play_album(ix, cx);
                        } else if let Ok(key) = std::env::var("CRATE_ARTIST_KEY") {
                                this.open_artist(key, std::env::var("CRATE_ARTIST_NAME").unwrap_or_default(), cx);
                        } else {
                            let find = |var: &str| {
                                let key = std::env::var(var).ok()?;
                                this.albums.iter().position(|a| a.rating_key == key)
                            };
                            let (open, play) = (find("CRATE_OPEN_KEY"), find("CRATE_AUTOPLAY_KEY"));
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
                            this.theme_gen = gen_now.clone();
                            // Crate's own palette doesn't follow Omarchy switches.
                            if this.theme_mode == theme::Mode::Crate {
                                return;
                            }
                            let (t, g) = Theme::load(theme::Mode::Omarchy);
                            this.marks.borrow_mut().clear();
                            eprintln!("[crate] theme changed -> accent {:06x}", t.accent);
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

        let total = std::env::var("CRATE_ITEMS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0); // 0 = the real library size
        let bench = std::env::var("CRATE_BENCH").ok().map(|_| Bench {
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
            theme_mode,
            marks: std::cell::RefCell::new(art::Marks::default()),
            albums: Vec::new(),
            total,
            status: "Loading library…".into(),
            status_seen: "Loading library…".into(),
            status_until: None,
            play_note: None,
            leveling: theme::saved_leveling(),
            gains: HashMap::new(),
            fade_secs: theme::saved_fade(),
            remote_kbps: theme::saved_remote_kbps(),
            eq_preset,
            eq_custom,
            eq_sliders,
            eq_save_gen: 0,
            settings_scroll: {
                let h = gpui::ScrollHandle::new();
                // Test hook: open Settings scrolled to the bottom.
                if std::env::var_os("CRATE_SETTINGS_BOTTOM").is_some() {
                    h.scroll_to_bottom();
                }
                h
            },
            pending_start: None,
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
            target: std::env::var("CRATE_TARGET").ok(),
            engine: None,
            now: None,
            autoplay: std::env::var("CRATE_AUTOPLAY").ok().and_then(|v| v.parse().ok()),
            play_gen: 0,
            view: View::Home,
            back: Vec::new(),
            track_detail: HashMap::new(),
            content_w: 1000.0,
            queue_seq: 0,
            play_intent: 0,
            shuffle: false,
            repeat: queue::Repeat::Off,
            volume: theme::saved_volume(),
            unmute_to: if theme::saved_volume() > 0.0 { theme::saved_volume() } else { 0.5 },
            volume_gen: 0,
            volume_scroll: 0.0,
            theme_previews: None,
            lib_view: None,
            lib_filters: Vec::new(),
            lib_sort: library::LibSort::Added,
            lib_gen: 0,
            card_w: 140.0,
            lib_list: false,
            selection: Vec::new(),
            palette: false,
            pal_sel: 0,
            pal_scope: 0,
            search_done: 0,
            test_search: std::env::var("CRATE_SEARCH").ok(),
            cursor: None,
            compact: false,
            test_keys: std::env::var("CRATE_KEYS").ok(),
            now_playing: std::env::var_os("CRATE_NOW_PLAYING").is_some(),
            sonic_progress: None,
            compact_playing: true,
            audio_info: None,
            prompt_copied: false,
            raise_requested: false,
            // Albums left half-downloaded last time pick up where they stopped.
            offline: offline::Offline::new(),
            sonic_albums: HashMap::new(),
            edition_formats: HashMap::new(),
            appears_on: HashMap::new(),
            guest_dj: std::env::var_os("CRATE_GUEST_DJ").is_some(),
            dj_keys: std::collections::HashSet::new(),
            dj_done: std::collections::HashSet::new(),
            dj_retry: Vec::new(),
            dj_seen: None,
            sonic_artists: HashMap::new(),
            sonic: sonic::SonicState::default(),
            artist_kinds: HashMap::new(),
            artist_tabs: HashMap::new(),
            sonic_search,
            history: None,
            top_tracks: None,
            artist_cache: Vec::new(),
            artist_cache_for: usize::MAX,
            artist_scroll: UniformListScrollHandle::new(),
            np_tab: std::env::var("CRATE_NP_TAB").ok().and_then(|v| v.parse().ok()).unwrap_or(0),
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
            _on_quit: Some(on_quit),
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
        // `#` (genres) and `>` (commands) are local; `@` searches artists.
        let (prefix, rest) = palette::split_prefix(&self.search_query);
        if matches!(prefix, Some('#' | '>')) || rest.is_empty() {
            self.hits.clear();
            cx.notify();
            return;
        }
        let query = rest.to_string();
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
            let res = rx.await;
            // Finished with an error (or no reply): the palette stops saying "Searching…".
            if !matches!(res, Ok(Ok(_))) {
                this.update(cx, |this, cx| {
                    if this.search_gen == gen_id {
                        this.search_done = gen_id;
                        cx.notify();
                    }
                })
                .ok();
            }
            if let Ok(Ok(hits)) = res {
                eprintln!("[crate] search {query:?}: {} hits in {:?}", hits.len(), t0.elapsed());
                this.update(cx, |this, cx| {
                    if this.search_gen == gen_id {
                        for h in &hits {
                            if !h.thumb.is_empty() && !this.art.contains_key(&h.thumb) && this.requested.insert(h.thumb.clone()) {
                                let _ = this.art_tx.send(ArtRequest { thumb: h.thumb.clone() });
                            }
                        }
                        this.hits = hits;
                        this.search_done = gen_id;
                        cx.notify();
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    fn activate_hit(&mut self, h: SearchHit, cx: &mut Context<Self>) {
        eprintln!("[crate] activate {} {:?}", h.kind, h.title);
        // Opening a page shows it: close Now Playing (a track just plays).
        if h.kind != "track" {
            self.now_playing = false;
        }
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
                Ok(Err(e)) => {
                    eprintln!("[crate] tracks: {e:#}");
                    let title = album.title.clone();
                    this.update(cx, |this, cx| {
                        this.status = format!("Couldn't load {title}: {e:#}").into();
                        cx.notify();
                    })
                    .ok();
                }
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
        self.request_sonic_artists(key.clone(), cx);
        self.request_artist_kinds(key.clone(), cx);
        self.album_scroll = gpui::ScrollHandle::new();
        self.cursor = None;
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
                    this.request_appears_on(key.clone(), info.title.clone(), cx);
                    if let View::Artist { key: k, info: slot, .. } = &mut this.view
                        && *k == key
                    {
                        *slot = Some(info);
                        cx.notify();
                    }
                })
                .ok();
            }
            Ok(Err(e)) => eprintln!("[crate] artist: {e:#}"),
            Err(_) => {}
        })
        .detach();
    }

    /// Play `track_key` in the context of its album.
    fn play_track_in_album(&mut self, album_key: &str, track_key: String, cx: &mut Context<Self>) {
        let Some(album) = self.albums.iter().find(|a| a.rating_key == album_key).cloned() else { return };
        self.request_art(&album);
        self.play_intent += 1;
        let intent = self.play_intent;
        self.with_tracks(album, cx, move |this, album, tracks, cx| {
            // Something else was played since the click.
            if this.play_intent != intent {
                return;
            }
            let start = tracks.iter().position(|t| t.rating_key == track_key).unwrap_or(0);
            this.play_tracks(album, tracks, start, cx);
        });
    }

    /// Navigate, recording history.
    fn go(&mut self, v: View) {
        let old = std::mem::replace(&mut self.view, v);
        self.compact_playing = false;
        // A selection belongs to the Albums page it was made on.
        if !matches!(self.view, View::Library) {
            self.selection.clear();
        }
        self.back.push(old);
        if self.back.len() > 50 {
            self.back.remove(0);
        }
        self.fwd.clear();
        self.album_scroll = gpui::ScrollHandle::new();
        self.cursor = None;
    }

    fn go_back(&mut self, cx: &mut Context<Self>) {
        if let Some(v) = self.back.pop() {
            let cur = std::mem::replace(&mut self.view, v);
            self.fwd.push(cur);
            self.album_scroll = gpui::ScrollHandle::new();
        self.cursor = None;
            if !matches!(self.view, View::Library) {
                self.selection.clear();
            }
            self.reload_view(cx);
            cx.notify();
        }
    }

    /// A view restored from history may still be waiting on data whose reply
    /// arrived after we'd left it; ask again.
    fn reload_view(&mut self, cx: &mut Context<Self>) {
        // A similar-albums lookup that failed on the first visit gets another try.
        if let View::Album { ix, .. } = &self.view
            && let Some(k) = self.album(*ix).map(|a| a.rating_key.clone())
        {
            self.request_sonic_albums(k, cx);
        }
        match &self.view {
            View::Album { ix, tracks: None } => {
                let ix = *ix;
                self.load_album_tracks(ix, cx);
            }
            View::Artist { key, info, .. } => {
                let (key, missing) = (key.clone(), info.is_none());
                let title = info.as_ref().map(|i| i.title.clone());
                // Lookups that failed on the first visit get another try.
                self.request_artist_kinds(key.clone(), cx);
                self.request_sonic_artists(key.clone(), cx);
                if let Some(title) = title {
                    self.request_appears_on(key.clone(), title, cx);
                }
                if missing {
                    self.load_artist_info(key, cx);
                }
            }
            _ => {}
        }
    }

    fn go_forward(&mut self, cx: &mut Context<Self>) {
        if let Some(v) = self.fwd.pop() {
            let cur = std::mem::replace(&mut self.view, v);
            self.back.push(cur);
            self.album_scroll = gpui::ScrollHandle::new();
        self.cursor = None;
            if !matches!(self.view, View::Library) {
                self.selection.clear();
            }
            self.reload_view(cx);
            cx.notify();
        }
    }

    fn open_album(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(album) = self.album(ix).cloned() else { return };
        self.request_art(&album);
        self.go(View::Album { ix, tracks: None });
        self.request_album_detail(album.rating_key.clone(), cx);
        self.request_sonic_albums(album.rating_key.clone(), cx);
        self.request_edition_formats(&album, cx);
        self.album_scroll = gpui::ScrollHandle::new();
        self.cursor = None;
        cx.notify();
        self.load_album_tracks(ix, cx);
    }

    fn load_album_tracks(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(album) = self.album(ix).cloned() else { return };
        self.with_tracks(album, cx, move |this, _album, tracks, cx| {
            if let View::Album { ix: cur, tracks: slot } = &mut this.view
                && *cur == ix {
                    // Test hook: behave as if track N (1-based) was clicked.
                    let auto = std::env::var("CRATE_PLAY_TRACK").ok().and_then(|v| v.parse::<usize>().ok());
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
        self.play_intent += 1;
        let intent = self.play_intent;
        self.with_tracks(album, cx, move |this, album, tracks, cx| {
            if this.play_intent == intent && !tracks.is_empty() {
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
        // A note is for one named play; any other queue drops it.
        if self.play_note.as_ref().is_some_and(|(ctx, _)| *ctx != context) {
            self.play_note = None;
        }
        self.report_stop();
        let orig_order: Vec<String> = tracks.iter().map(|t| t.rating_key.clone()).collect();
        let mut tracks = tracks;
        if self.shuffle {
            crate::shuffle(&mut tracks[start + 1..]);
        }
        for t in &tracks {
            self.request_thumb(&t.thumb.clone());
        }
        self.now = Some(Now {
            context,
            context_key,
            tracks,
            formats: HashMap::new(),
            transcoded: HashMap::new(),
            level: HashMap::new(),
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
            rep_idx: None,
            rep_state: "stopped",
            rep_elapsed: 0,
            rep_at: Instant::now(),
            orig_order,
        });
        // With leveling on, the first track needs its gain before it starts
        // (bounded: Plex answers in tens of ms on a LAN; give up after 1.5 s).
        let tracks = self.now.as_ref().map(|n| n.tracks.clone()).unwrap_or_default();
        // Wait only if the tracks about to play are missing gains; earlier ones fill in behind.
        let mut missing = self.missing_gains(&tracks[start.min(tracks.len())..(start + 50).min(tracks.len())], 0);
        if missing.is_empty() {
            self.play_from(start, 0, cx);
            if self.leveling != theme::Leveling::Off {
                self.fill_gains(&tracks, start, cx);
            }
            return;
        }
        // Wait only for the first batch (the tracks about to play); the rest fill in behind.
        missing.truncate(50);
        let seq = self.queue_seq;
        // The old queue's audio stops now, not when the new one starts, and its
        // poll loop ends so its events can't land on the new queue.
        self.engine = None;
        self.play_gen += 1;
        self.pending_start = Some(seq);
        let rx = plex_call(move |c| async move {
            tokio::time::timeout(Duration::from_millis(1500), c.track_gains(missing)).await.map_err(|_| anyhow::anyhow!("timed out"))?
        });
        cx.spawn(async move |this, cx| {
            let res = rx.await;
            this.update(cx, |this, cx| {
                match res {
                    Ok(Ok(found)) => this.gains.extend(found),
                    Ok(Err(e)) => eprintln!("[crate] loudness: {e:#}"),
                    Err(_) => {}
                }
                // Start only if this queue is still the one waiting (nothing stopped,
                // paused, skipped or replaced it meanwhile).
                if this.pending_start == Some(seq) && this.engine.is_none() {
                    this.play_from(start, 0, cx);
                }
                // Either way, while it's the current queue: load the rest and retune
                // the engine's unopened tracks (it may have been started by Play).
                if let Some(n) = this.now.as_ref().filter(|n| n.queue_id == seq) {
                    let (rest, idx) = (n.tracks.clone(), n.idx);
                    // Apply the gains just received now, not after the rest load.
                    this.sync_upcoming();
                    this.fill_gains(&rest, idx, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Track keys among `tracks` whose loudness isn't known yet (none when
    /// leveling is off), from `from` onward first, then the ones before it.
    fn missing_gains(&self, tracks: &[Track], from: usize) -> Vec<String> {
        if self.leveling == theme::Leveling::Off {
            return Vec::new();
        }
        let from = from.min(tracks.len());
        let mut seen = std::collections::HashSet::new();
        tracks[from..]
            .iter()
            .chain(&tracks[..from])
            .filter(|t| !self.gains.contains_key(&t.rating_key) && seen.insert(t.rating_key.as_str()))
            .map(|t| t.rating_key.clone())
            .collect()
    }

    /// Fetch loudness for `tracks` in the background, then re-hand the engine
    /// its unopened tracks with their gains (the playing one keeps its gain).
    fn fill_gains(&mut self, tracks: &[Track], from: usize, cx: &mut Context<Self>) {
        let missing = self.missing_gains(tracks, from);
        if missing.is_empty() {
            self.sync_upcoming();
            return;
        }
        let rx = plex_call(move |c| async move { c.track_gains(missing).await });
        cx.spawn(async move |this, cx| {
            if let Ok(res) = rx.await {
                this.update(cx, |this, _| {
                    match res {
                        Ok(found) => this.gains.extend(found),
                        Err(e) => eprintln!("[crate] loudness: {e:#}"),
                    }
                    this.sync_upcoming();
                })
                .ok();
            }
        })
        .detach();
    }

    /// The curve the engine should run (all zero when Off).
    pub(crate) fn eq_gains(&self) -> [f32; 10] {
        match self.eq_preset {
            theme::EqPreset::Off => [0.0; 10],
            theme::EqPreset::Custom => self.eq_custom,
            p => p.curve().unwrap_or([0.0; 10]),
        }
    }

    fn apply_eq(&mut self, save: bool, cx: &mut Context<Self>) {
        if let Some(e) = &self.engine {
            e.set_eq(self.eq_gains());
        }
        if save && let Err(e) = theme::save_eq(self.eq_preset, &self.eq_custom) {
            self.status = format!("Couldn't save the EQ: {e}").into();
        }
        cx.notify();
    }

    /// A preset chip: load its curve into the sliders (Off keeps them as they are).
    pub(crate) fn set_eq_preset(&mut self, p: theme::EqPreset, window: &mut Window, cx: &mut Context<Self>) {
        self.eq_preset = p;
        let shown = p.curve().unwrap_or(self.eq_custom);
        for (s, g) in self.eq_sliders.iter().zip(shown) {
            s.update(cx, |s, cx| s.set_value(g, window, cx));
        }
        self.apply_eq(true, cx);
    }

    /// A slider moved: that makes the curve Custom (starting from what was shown).
    fn set_eq_band(&mut self, i: usize, db: f32, save: bool, cx: &mut Context<Self>) {
        if let Some(curve) = self.eq_preset.curve() {
            self.eq_custom = curve;
        }
        self.eq_preset = theme::EqPreset::Custom;
        if let Some(g) = self.eq_custom.get_mut(i) {
            *g = if db.is_finite() { db.clamp(-12.0, 12.0) } else { 0.0 };
        }
        self.apply_eq(save, cx);
        if !save {
            // No release for keyboard/assistive steps: save once things settle.
            self.eq_save_gen += 1;
            let gen_id = self.eq_save_gen;
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(800)).await;
                this.update(cx, |this, cx| {
                    if this.eq_save_gen == gen_id {
                        this.apply_eq(true, cx);
                    }
                })
                .ok();
            })
            .detach();
        }
    }

    pub(crate) fn set_fade(&mut self, secs: u32, cx: &mut Context<Self>) {
        if self.fade_secs == secs {
            return;
        }
        self.fade_secs = secs;
        if let Err(e) = theme::save_fade(secs) {
            self.status = format!("Couldn't save the fade setting: {e}").into();
        }
        // Retune the engine's unopened tracks (the next transition onward).
        self.sync_upcoming();
        cx.notify();
    }

    pub(crate) fn set_remote_quality(&mut self, kbps: u32, cx: &mut Context<Self>) {
        if self.remote_kbps == kbps {
            return;
        }
        self.remote_kbps = kbps;
        if let Err(e) = theme::save_remote_kbps(kbps) {
            self.status = format!("Couldn't save the streaming quality: {e}").into();
        }
        // The next track onward; the one playing keeps its stream.
        self.sync_upcoming();
        cx.notify();
    }

    pub(crate) fn set_leveling(&mut self, l: theme::Leveling, cx: &mut Context<Self>) {
        if self.leveling == l {
            return;
        }
        self.leveling = l;
        if let Err(e) = theme::save_leveling(l) {
            self.status = format!("Couldn't save the leveling setting: {e}").into();
        }
        let (tracks, idx) = self.now.as_ref().map(|n| (n.tracks.clone(), n.idx)).unwrap_or_default();
        self.fill_gains(&tracks, idx, cx);
        cx.notify();
    }

    fn art_path(album_key: &str) -> std::path::PathBuf {
        std::path::PathBuf::from(format!("/scratch/crate-cache/art-{album_key}.jpg"))
    }

    /// (Re)start the engine at album track `idx`, `ms` into it.
    fn play_from(&mut self, idx: usize, ms: u64, cx: &mut Context<Self>) {
        // An explicit start supersedes a queue waiting on loudness.
        self.pending_start = None;
        // The new engine reports its own gains; don't show the old one's meanwhile.
        if let Some(n) = self.now.as_mut() {
            n.level.clear();
            n.transcoded.clear();
            n.formats.clear();
        }
        let Some(now) = self.now.as_mut() else { return };
        if idx >= now.tracks.len() {
            return;
        }
        // Restarting or seeking back within the reported track starts a new
        // listen: close the old one first so a finished listen still counts.
        if now.rep_idx == Some(idx) && ms < now.elapsed_ms && now.rep_state != "stopped" {
            send_timeline(now.tracks[idx].rating_key.clone(), "stopped", now.elapsed_ms, now.tracks[idx].duration_ms);
            now.rep_state = "stopped";
            now.rep_elapsed = 0;
        }
        self.engine = None; // dropping stops the old one
        let range = self.engine_slice(&self.now.as_ref().map(|n| n.tracks.clone()).unwrap_or_default(), idx);
        let queue = self.sources(&self.now.as_ref().map(|n| n.tracks[range.clone()].to_vec()).unwrap_or_default(), None);
        let eq = self.eq_gains();
        let Some(now) = self.now.as_mut() else { return };
        match Engine::play_from(queue, ms, self.target.clone()) {
            Ok(e) => {
                e.set_volume(self.volume.powi(3));
                e.set_eq(eq);
                self.engine = Some(e);
                now.start_idx = idx;
                now.start_ms = ms;
                now.boundaries.clear();
                now.idx = idx;
                now.elapsed_ms = ms;
                now.paused = false;
                now.finished = false;
                now.stopped = false;
                now.engine_end = range.end;
                // A note for the play that just started (e.g. which marked album seeded radio).
                match self.play_note.take() {
                    Some((ctx, note)) if ctx == now.context => self.status = format!("Playing {} · {note}", now.context).into(),
                    // Seeks and resumes (ms > 0) restart the engine too; only a fresh start says so.
                    _ if ms == 0 => self.status = format!("Playing {}", now.context).into(),
                    _ => {}
                }
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
        // These work with nothing playing.
        match t {
            Transport::Raise => {
                // Ask Hyprland now: a hidden window may not render to act on the flag.
                instance::focus_hint();
                self.raise_requested = true;
                cx.notify();
                return;
            }
            Transport::SetVolume(v) => return self.set_volume(v, cx),
            Transport::SetShuffle(on) => return self.set_modes(Some(on), None, cx),
            Transport::SetRepeat(r) => return self.set_modes(None, Some(r), cx),
            _ => {}
        }
        // Pause and Stop cancel a queue still waiting on loudness; anything that
        // starts playback goes through play_from, which supersedes it. A no-op
        // (Next at the end) leaves the wait alone.
        if matches!(t, Transport::Stop | Transport::Pause) {
            self.pending_start = None;
        }
        let Some(now) = self.now.as_mut() else { return };
        let (idx, elapsed, n) = (now.idx, now.elapsed_ms, now.tracks.len());
        eprintln!("[crate] transport {t:?}");
        match t {
            Transport::PlayPause | Transport::Play | Transport::Pause => {
                let want_pause = match t {
                    Transport::Play => false,
                    Transport::Pause => true,
                    _ => !now.paused,
                };
                if now.finished {
                    // After a finish (paused stays false), PlayPause means play again:
                    // from the top at the end of the queue, else from the current
                    // track (an error or a radio refill that ran dry mid-queue).
                    if !matches!(t, Transport::Pause) {
                        let from = if idx + 1 >= n { 0 } else { idx };
                        self.play_from(from, 0, cx);
                    }
                    return;
                }
                if want_pause {
                    self.pending_start = None;
                }
                now.paused = want_pause;
                if let Some(e) = &self.engine {
                    e.set_active(!want_pause);
                } else if !want_pause {
                    // No engine (a queue still waiting on loudness, or paused while
                    // waiting): play starts it here.
                    self.play_from(idx, elapsed, cx);
                    return;
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
            Transport::SetShuffle(_) | Transport::SetRepeat(_) | Transport::SetVolume(_) | Transport::Raise => {}
            Transport::Stop => {
                self.engine = None;
                now.finished = true;
                now.stopped = true;
                self.report_stop();
                let Some(now) = self.now.as_mut() else { return };
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
                AudioEvent::TrackStarted { index, label, source_rate, bits, at_us, gain, .. } => {
                    // The engine's queue must match ours, or the UI shows the wrong track.
                    let want = now.tracks.get(now.start_idx + index).and_then(|t| t.part_key.split('/').nth(3)).unwrap_or("?");
                    if !label.contains(&format!("part-{want}.")) {
                        eprintln!("[crate] queue mismatch at {}: engine opened {label}, expected part {want}", now.start_idx + index);
                    } else if std::env::var_os("CRATE_TRACE").is_some() {
                        eprintln!("[crate] engine opened {label} for #{}", now.start_idx + index);
                    }
                    now.formats.insert(now.start_idx + index, (source_rate, bits));
                    match transcoded_kbps(&label) {
                        Some(k) => now.transcoded.insert(now.start_idx + index, k),
                        None => now.transcoded.remove(&(now.start_idx + index)),
                    };
                    // What the decoder really applies to this playback (not what we last asked for).
                    now.level.insert(now.start_idx + index, gain);
                    // A skipped track before this one gets a zero-length slot.
                    while now.boundaries.len() < index {
                        now.boundaries.push(at_us);
                    }
                    if now.boundaries.len() == index {
                        now.boundaries.push(at_us);
                    }
                }
                AudioEvent::Skipped { index, error } => {
                    let title = now.tracks.get(now.start_idx + index).map(|t| t.title.clone()).unwrap_or_default();
                    eprintln!("[crate] skipped #{} {title}: {error}", now.start_idx + index);
                    self.status = format!("Skipped \u{201c}{title}\u{201d}: couldn't load it ({error})").into();
                }
                AudioEvent::Reconnected { from, to, silent_ms } => {
                    eprintln!("[crate] rate change {from} -> {to}: {silent_ms:.1} ms");
                }
                AudioEvent::Finished => now.finished = true,
                AudioEvent::Error(e) => {
                    eprintln!("[crate] audio: {e}");
                    self.status = format!("Audio error: {e}").into();
                    now.finished = true;
                    now.stopped = true;
                    failed = true;
                }
                AudioEvent::FirstAudio { ms_since_play } => {
                    eprintln!("[crate] first audio {ms_since_play:.0} ms");
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
        self.guest_dj_tick(cx);
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
                                // The engine may already have finished: repeat-all wraps now.
                                if now.finished && !now.stopped {
                                    this.repeat_on_finish(cx);
                                }
                                cx.notify();
                                return;
                            }
                            let thumbs: Vec<String> = more.iter().map(|t| t.thumb.clone()).collect();
                            now.tracks.extend(more.iter().cloned());
                            now.orig_order.extend(more.iter().map(|t| t.rating_key.clone()));
                            // Gapless: hand them to the running engine (unless it already finished,
                            // or repeat-one keeps it to the current track).
                            if this.repeat != queue::Repeat::One
                                && let Some(e) = this.engine.as_ref()
                                && let Some(n) = this.now.as_ref()
                                && n.engine_end == n.tracks.len() - more.len()
                                && e.append(this.sources(&more, n.tracks.len().checked_sub(more.len() + 1).and_then(|p| n.tracks.get(p))))
                                && let Some(n) = this.now.as_mut()
                            {
                                n.engine_end = n.tracks.len();
                                // Appended at unity if their loudness isn't known yet; retune once it is.
                                this.fill_gains(&more, 0, cx);
                            }
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
                            eprintln!("[crate] refill: {e:#}");
                            now.refill = None;
                            if now.finished && !now.stopped {
                                this.repeat_on_finish(cx);
                            }
                        }
                        Err(_) => {}
                    }
                })
                .ok();
            })
            .detach();
        }
        // The engine reached the end of its queue but the queue has grown.
        if now.finished && !failed && !now.stopped && now.engine_end < now.tracks.len() && self.repeat != queue::Repeat::One {
            let from = now.engine_end;
            self.play_from(from, 0, cx);
            return false;
        }
        if now.finished && !failed && self.repeat_on_finish(cx) {
            return false;
        }
        let Some(now) = self.now.as_mut() else { return false };
        let _ = self.mpris.send(MprisUpdate::Position(now.elapsed_ms));
        if now.finished {
            let _ = self.mpris.send(MprisUpdate::Status { playing: false, stopped: true });
        }
        cx.notify();
        let keep = !now.finished;
        self.report();
        if failed {
            // Stop the pipeline; Drop stops both engine threads.
            self.engine = None;
        }
        keep
    }

    /// Grid cells: the real library, or CRATE_ITEMS for the scroll benchmark.
    /// Tell the server what's playing: `playing`/`paused` on changes and every
    /// 10 s, `stopped` when a track ends or is left. The server records the
    /// play (and scrobbles) when a track stops past ~90%.
    fn report(&mut self) {
        let Some(n) = self.now.as_mut() else { return };
        let Some(tr) = n.tracks.get(n.idx) else { return };
        let state = if n.finished { "stopped" } else if n.paused { "paused" } else { "playing" };
        let mut sends: Vec<(String, &'static str, u64, u64)> = Vec::new();
        if n.rep_idx != Some(n.idx) {
            // Left the previous track (finished, skipped or seeked away).
            if let Some(prev) = n.rep_idx.and_then(|i| n.tracks.get(i))
                && n.rep_state != "stopped"
            {
                sends.push((prev.rating_key.clone(), "stopped", n.rep_elapsed, prev.duration_ms));
            }
            n.rep_idx = Some(n.idx);
            n.rep_state = "stopped";
        }
        let due = n.rep_at.elapsed() >= Duration::from_secs(10);
        if state != n.rep_state || (state == "playing" && due) {
            if !(state == "stopped" && n.rep_state == "stopped") {
                sends.push((tr.rating_key.clone(), state, n.elapsed_ms, tr.duration_ms));
            }
            n.rep_state = state;
            n.rep_at = Instant::now();
        }
        n.rep_elapsed = n.elapsed_ms;
        for (rk, st, t, d) in sends {
            send_timeline(rk, st, t, d);
        }
    }

    /// The current track is being abandoned (stop, or a new queue): close it out.
    fn report_stop(&mut self) {
        let Some(n) = self.now.as_mut() else { return };
        if let Some(tr) = n.rep_idx.and_then(|i| n.tracks.get(i))
            && n.rep_state != "stopped"
        {
            let at = if n.rep_idx == Some(n.idx) { n.elapsed_ms } else { n.rep_elapsed };
            send_timeline(tr.rating_key.clone(), "stopped", at, tr.duration_ms);
            n.rep_state = "stopped";
        }
    }

    /// A generated mark (station art, placeholder) in the current theme.
    pub(crate) fn mark(&self, m: art::Motif) -> Arc<Image> {
        self.marks.borrow_mut().get(m, &self.theme)
    }

    /// Cover art, or the item's stable generated placeholder while it loads.
    pub(crate) fn cover_or_mark(&self, thumb: &str, key: &str, size: f32, round: f32) -> gpui::AnyElement {
        use gpui::ObjectFit;
        let image = self.art.get(thumb).cloned().unwrap_or_else(|| self.mark(art::placeholder_for(if key.is_empty() { thumb } else { key })));
        gpui::img(image).size(px(size)).flex_none().rounded(px(round)).object_fit(ObjectFit::Cover).into_any_element()
    }

    pub(crate) fn set_theme_mode(&mut self, mode: theme::Mode, cx: &mut Context<Self>) {
        self.theme_mode = mode;
        if let Err(e) = theme::save_mode(mode) {
            self.status = format!("Couldn't save the theme choice: {e}").into();
        }
        let (t, g) = Theme::load(mode);
        apply_component_theme(&t, None, cx);
        self.theme = t;
        self.theme_gen = g;
        self.marks.borrow_mut().clear();
        cx.notify();
    }

    /// Full Now Playing (lyrics are its middle column), optionally on a panel tab.
    fn open_now_playing(&mut self, tab: Option<usize>, cx: &mut Context<Self>) {
        if self.now.is_none() {
            return;
        }
        if let Some(t) = tab {
            self.np_tab = t;
        }
        self.now_playing = true;
        cx.notify();
    }

    /// Set volume (perceptual 0..=1): applied live, saved, reported to MPRIS.
    pub(crate) fn set_volume(&mut self, v: f32, cx: &mut Context<Self>) {
        if !v.is_finite() {
            return;
        }
        let v = (v.clamp(0.0, 1.0) * 100.0).round() / 100.0;
        if v > 0.0 {
            self.unmute_to = v;
        }
        self.volume = v;
        if let Some(e) = &self.engine {
            e.set_volume(v.powi(3));
        }
        let _ = self.mpris.send(MprisUpdate::Volume(v as f64));
        // Save once the slider settles, not on every scroll step.
        self.volume_gen += 1;
        let gen_id = self.volume_gen;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(600)).await;
            this.update(cx, |this, _| {
                if this.volume_gen == gen_id
                    && let Err(e) = theme::save_volume(this.volume)
                {
                    this.status = format!("Volume not saved: {e}").into();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn toggle_mute(&mut self, cx: &mut Context<Self>) {
        let to = if self.volume > 0.0 { 0.0 } else { self.unmute_to.max(0.05) };
        let keep = self.unmute_to;
        self.set_volume(to, cx);
        self.unmute_to = keep;
    }

    /// 1–5 keys: rate the playing track.
    fn rate_playing(&mut self, stars: u8, cx: &mut Context<Self>) {
        if let Some(rk) = self.now.as_ref().and_then(|n| n.tracks.get(n.idx)).map(|t| t.rating_key.clone()) {
            self.rate_track(rk, stars, cx);
        }
    }

    /// Rate a track 1–5 stars; choosing the current rating clears it.
    pub(crate) fn rate_track(&mut self, rk: String, stars: u8, cx: &mut Context<Self>) {
        let current = self
            .track_detail
            .get(&rk)
            .and_then(|d| d.as_ref())
            .and_then(|d| d.rating)
            .or_else(|| self.album_detail.values().flatten().find_map(|a| a.stats.get(&rk).and_then(|s| s.1)));
        // Clear only when the click matches the stored value exactly; a half-star
        // rating from another client (shown rounded down) gets set, not cleared.
        let new = if current == Some(f32::from(stars * 2)) { None } else { Some(stars * 2) };
        if let Some(Some(d)) = self.track_detail.get_mut(&rk) {
            d.rating = new.map(f32::from);
        }
        for a in self.album_detail.values_mut().flatten() {
            if let Some(s) = a.stats.get_mut(&rk) {
                s.1 = new.map(f32::from);
            }
        }
        self.rate_on_server(rk, new, cx);
    }

    pub(crate) fn rate_album(&mut self, key: String, stars: u8, cx: &mut Context<Self>) {
        let Some(Some(d)) = self.album_detail.get_mut(&key) else { return };
        let new = if d.rating == Some(f32::from(stars * 2)) { None } else { Some(stars * 2) };
        d.rating = new.map(f32::from);
        self.rate_on_server(key, new, cx);
    }

    fn rate_on_server(&mut self, key: String, rating: Option<u8>, cx: &mut Context<Self>) {
        cx.notify();
        queue_write(Write::Rate(key, rating));
    }

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
            Ok(Err(e)) => eprintln!("[crate] track detail: {e:#}"),
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
            eprintln!("[crate] bench: scrolling at {} px/s for {:?}", b.speed, b.duration);
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

impl Crate {

    fn open_hub_item(&mut self, kind: &str, key: &str, title: &str, cx: &mut Context<Self>) {
        match kind {
            "album" => {
                if let Some(ix) = self.albums.iter().position(|a| a.rating_key == key) {
                    self.open_album(ix, cx);
                }
            }
            "artist" => self.open_artist(key.to_string(), title.to_string(), cx),
            _ => eprintln!("[crate] station {title:?} not implemented yet"),
        }
    }


}

/// Fisher–Yates with a time-seeded xorshift (no rand dependency).
pub(crate) fn shuffle<T>(v: &mut [T]) {
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

impl Render for Crate {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t0 = Instant::now();
        let t = self.theme.clone();
        // Test hook: canvas key actions, 1.5 s apart, once the library is in.
        if !self.albums.is_empty()
            && let Some(keys) = self.test_keys.take()
        {
            cx.spawn(async move |this, cx| {
                for k in keys.split(',').map(str::trim).map(String::from).collect::<Vec<_>>() {
                    cx.background_executor().timer(Duration::from_millis(1500)).await;
                    let ok = this.update(cx, |this, cx| {
                        match k.as_str() {
                            "j" => this.move_cursor(1, cx),
                            "k" => this.move_cursor(-1, cx),
                            "enter" => this.activate_cursor(cx),
                            "x" => this.toggle_cursor_mark(cx),
                            "r" => this.radio_from_selection(cx),
                            "reroll" => this.reroll_sonic_path(cx),
                            "mix" => {
                                if let View::Artist { key, name, .. } = &this.view {
                                    let (k, n) = (key.clone(), name.clone());
                                    this.build_artist_mix(k, n, cx);
                                }
                            }
                            "bottom" => {
                                this.album_scroll.scroll_to_bottom();
                                this.settings_scroll.scroll_to_bottom();
                                cx.notify();
                            }
                            k if k.starts_with("next:") || k.starts_with("queue:") => {
                                let (next, key) = k.split_once(':').map(|(a, b)| (a == "next", b.to_string())).unwrap_or((false, String::new()));
                                if let Some(album) = this.albums.iter().find(|a| a.rating_key == key).cloned() {
                                    let label = album.title.clone();
                                    this.with_tracks(album, cx, move |this, _a, tracks, cx| this.enqueue(tracks, next, label, cx));
                                }
                            }
                            k if k.starts_with("seek:") => {
                                if let Ok(ms) = k[5..].parse() {
                                    this.transport(Transport::SetPosition(ms), cx);
                                }
                            }
                            k if k.starts_with("play:") => {
                                if let Some(ix) = this.albums.iter().position(|a| a.rating_key == k[5..]) {
                                    this.play_album(ix, cx);
                                }
                            }
                            _ => eprintln!("[crate] CRATE_KEYS: unknown {k}"),
                        }
                        eprintln!("[crate] key {k}: cursor {:?} selection {}", this.cursor, this.selection.len());
                        if std::env::var_os("CRATE_TRACE").is_some()
                            && let Some(p) = &this.sonic.path
                        {
                            eprintln!("[crate] sonic path: {:?}", p.iter().map(|t| t.title.chars().take(12).collect::<String>()).collect::<Vec<_>>());
                        }
                    });
                    if ok.is_err() {
                        break;
                    }
                }
            })
            .detach();
        }
        // Test hook: open the palette with a query once the library is in.
        if !self.albums.is_empty()
            && let Some(q) = self.test_search.take()
        {
            self.search.update(cx, |s, cx| s.set_value(q.clone(), window, cx));
            self.open_palette(window, cx);
            self.on_search_change(q, cx);
        }
        if std::mem::take(&mut self.raise_requested) {
            window.activate_window();
        }
        if std::mem::take(&mut self.reset_search) {
            self.search.update(cx, |s, cx| s.set_value("", window, cx));
            self.search_query.clear();
            window.focus(&self.focus, cx);
        }
        let vw: f32 = window.viewport_size().width.into();
        // Narrow tiled window: the canvas Tile layout (no sidebar, bottom nav).
        let compact = vw < 720.0;
        self.compact = compact;
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

        let grid = if self.lib_list {
            uniform_list(
                "albums-list",
                self.cells(),
                cx.processor(|this, range: std::ops::Range<usize>, _window, cx| range.map(|i| this.list_row(i, cx)).collect::<Vec<_>>()),
            )
            .track_scroll(&self.scroll)
            .flex_1()
        } else {
            uniform_list(
                "albums",
                rows,
                cx.processor(|this, range: std::ops::Range<usize>, _window, cx| range.map(|r| this.row(r, cx).into_any_element()).collect::<Vec<_>>()),
            )
            .track_scroll(&self.scroll)
            .flex_1()
        };
        let sel_bar = self.selection_bar(&t, cx);
        // Keep the last rows reachable above the floating bar.
        let grid = if sel_bar.is_some() { grid.pb(px(72.0)) } else { grid };
        let grid = div().flex_1().min_h_0().flex().relative().child(grid).children(if self.lib_list { None } else { rail }).children(sel_bar);

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
            View::Soon("offline") => self.offline_view(&t, cx),
            View::Soon("sonic") => self.sonic_view(&t, cx),
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

        let toast = self.status_toast(&t, cx);
        let el = div()
            .key_context("Crate")
            .track_focus(&self.focus)
            .capture_key_down(|ev: &gpui::KeyDownEvent, window, cx| {
                if std::env::var_os("CRATE_KEYLOG").is_some() {
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
                if !this.selection.is_empty() && matches!(this.view, View::Library) && !this.now_playing {
                    this.selection.clear();
                    cx.notify();
                } else if this.now_playing {
                    this.now_playing = false;
                    cx.notify();
                } else {
                    this.go_back(cx)
                }
            }))
            .on_action(cx.listener(|this, _: &GoHome, window, cx| {
                this.now_playing = false;
                this.nav_to("home", window, cx)
            }))
            .on_action(cx.listener(|this, _: &GoStations, window, cx| {
                this.now_playing = false;
                this.nav_to("stations", window, cx)
            }))
            .on_action(cx.listener(|this, _: &ShowLyrics, _, cx| this.open_now_playing(None, cx)))
            .on_action(cx.listener(|this, _: &VolumeUp, _, cx| this.set_volume(this.volume + 0.05, cx)))
            .on_action(cx.listener(|this, _: &VolumeDown, _, cx| this.set_volume(this.volume - 0.05, cx)))
            .on_action(cx.listener(|this, _: &ShowQueue, _, cx| this.open_now_playing(Some(0), cx)))
            .on_action(cx.listener(|this, _: &CursorNext, _, cx| this.move_cursor(1, cx)))
            .on_action(cx.listener(|this, _: &CursorPrev, _, cx| this.move_cursor(-1, cx)))
            .on_action(cx.listener(|this, _: &ActivateCursor, _, cx| this.activate_cursor(cx)))
            .on_action(cx.listener(|this, _: &MarkCursor, _, cx| this.toggle_cursor_mark(cx)))
            .on_action(cx.listener(|this, _: &RadioFromSelection, _, cx| this.radio_from_selection(cx)))
            .on_action(cx.listener(|this, _: &Rate1, _, cx| this.rate_playing(1, cx)))
            .on_action(cx.listener(|this, _: &Rate2, _, cx| this.rate_playing(2, cx)))
            .on_action(cx.listener(|this, _: &Rate3, _, cx| this.rate_playing(3, cx)))
            .on_action(cx.listener(|this, _: &Rate4, _, cx| this.rate_playing(4, cx)))
            .on_action(cx.listener(|this, _: &Rate5, _, cx| this.rate_playing(5, cx)))
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
            .children(toast)
            .children(palette);
        self.last_render_us = t0.elapsed().as_micros();
        el
    }
}

impl Crate {
    /// Art for "Mixes for you": the mix artists' album covers and the mood
    /// mixes' covers. Called when Home, the album list or mix covers arrive.
    fn request_mix_art(&mut self) {
        let Some(home) = self.home.as_ref() else { return };
        let mut thumbs: Vec<String> = Crate::mix_artists(home)
            .iter()
            .flat_map(|(k, _)| self.albums.iter().filter(move |a| a.artist_key == *k).take(4).map(|a| a.thumb.clone()))
            .collect();
        thumbs.extend(home.mixes.iter().flat_map(|m| m.3.iter().map(|(th, _)| th.clone())));
        // Collage filler: an album cover per sonically similar artist.
        for (k, _) in Crate::mix_artists(home) {
            if let Some(Some(sim)) = self.sonic_artists.get(&k) {
                thumbs.extend(sim.iter().filter_map(|s| self.albums.iter().find(|a| a.artist_key == s.0)).map(|a| a.thumb.clone()));
            }
        }
        for th in thumbs {
            self.request_thumb(&th);
        }
    }

    /// Show `msg` as a toast even if it's the same text as last time.
    pub(crate) fn say(&mut self, msg: impl Into<SharedString>) {
        self.status = msg.into();
        self.status_seen = SharedString::default();
    }

    /// Status messages (errors, "Starting radio…", skipped albums) as a
    /// toast above the player bar for a few seconds after each change.
    fn status_toast(&mut self, t: &Theme, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        const SHOW: Duration = Duration::from_secs(5);
        if self.status != self.status_seen {
            self.status_seen = self.status.clone();
            if self.status.is_empty() {
                self.status_until = None;
            } else {
                self.status_until = Some(Instant::now() + SHOW);
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(SHOW + Duration::from_millis(50)).await;
                    this.update(cx, |_, cx| cx.notify()).ok();
                })
                .detach();
            }
        }
        if self.status_until.is_none_or(|u| Instant::now() >= u) {
            return None;
        }
        Some(
            div()
                .absolute()
                .left_0()
                .right_0()
                // Above the player bar, or near the top over Now Playing (its controls sit low).
                .when(self.now_playing && self.now.is_some() && !self.compact, |d| d.top(px(72.0)))
                .when(!(self.now_playing && self.now.is_some() && !self.compact), |d| d.bottom(px(104.0)))
                .flex()
                .justify_center()
                .child(
                    div()
                        .id("status-toast")
                        .max_w(px(560.0))
                        .px(px(16.0))
                        .py(px(9.0))
                        .rounded(px(10.0))
                        .bg(rgb(t.raised))
                        .border_1()
                        .border_color(rgb(t.line))
                        .text_size(px(13.0))
                        .text_color(rgb(t.text))
                        .shadow_lg()
                        .child(self.status.clone()),
                )
                .into_any_element(),
        )
    }
}

/// The music library section key for this account.
static SECTION: std::sync::OnceLock<String> = std::sync::OnceLock::new();
/// Whether the server address in use is remote (set once at launch).
pub(crate) static REMOTE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// The bitrate in a transcoded stream's engine label (`…part-<id>.tx<kbps>.mp3`).
fn transcoded_kbps(label: &str) -> Option<u32> {
    let rest = label.rsplit_once(".tx")?.1;
    rest.strip_suffix(".mp3")?.parse().ok()
}

pub(crate) fn section() -> String {
    SECTION.get().cloned().unwrap_or_else(|| "1".into())
}

fn load_fonts(cx: &mut App) {
    // Static Inter weights: GPUI on Linux renders a variable font at one weight.
    cx.text_system()
        .add_fonts(vec![
            std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/Inter-Regular.ttf").as_slice()),
            std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/Inter-Medium.ttf").as_slice()),
            std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/Inter-SemiBold.ttf").as_slice()),
            std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/Inter-Bold.ttf").as_slice()),
        ])
        .expect("bundled fonts");
}

/// No account yet: the sign-in window. It relaunches Crate when done.
fn run_setup(rt: tokio::runtime::Runtime) {
    let handle = rt.handle().clone();
    std::thread::spawn(move || rt.block_on(std::future::pending::<()>()));
    gpui_platform::application().with_assets(icons::Icons).run(move |cx: &mut App| {
        gpui_component::init(cx);
        load_fonts(cx);
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        let (t, _) = Theme::load(theme::saved_mode());
        apply_component_theme(&t, None, cx);
        let bounds = Bounds::centered(None, size(px(760.0), px(560.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                app_id: Some("crate".into()),
                titlebar: Some(gpui::TitlebarOptions { title: Some("Crate — sign in".into()), ..Default::default() }),
                ..Default::default()
            },
            move |window, cx| {
                let view = cx.new(|cx| setup::Setup::new(window, cx, handle.clone()));
                cx.new(|cx| gpui_component::Root::new(view, window, cx))
            },
        )
        .unwrap();
        cx.activate(true);
    });
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
/// Writes to Plex (timeline reports, ratings) go out one at a time, in
/// order: if a new track's `playing` overtook the previous track's `stopped`,
/// the server would close the old session itself and then count the late
/// `stopped` as a second play; two quick rating clicks must land in order.
enum Write {
    Timeline(String, &'static str, u64, u64),
    Rate(String, Option<u8>),
}

static WRITES: std::sync::OnceLock<mpsc::UnboundedSender<Write>> = std::sync::OnceLock::new();
/// Writes queued or in flight, so quitting can wait for them.
static PENDING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn queue_write(w: Write) {
    let tx = WRITES.get_or_init(|| {
        let (tx, mut rx) = mpsc::unbounded_channel::<Write>();
        if let Some((handle, client)) = PLEX.get() {
            let client = client.clone();
            handle.spawn(async move {
                while let Some(w) = rx.recv().await {
                    let r = match &w {
                        Write::Timeline(rk, st, t, d) => client.timeline(rk, st, *t, *d).await,
                        Write::Rate(k, r) => client.rate(k, *r).await,
                    };
                    if let Err(e) = r {
                        eprintln!("[crate] plex write: {e:#}");
                    }
                    PENDING.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                }
            });
        }
        tx
    });
    PENDING.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    if tx.send(w).is_err() {
        PENDING.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

fn send_timeline(rk: String, state: &'static str, time: u64, duration: u64) {
    if rk.is_empty() || std::env::var_os("CRATE_NO_REPORT").is_some() {
        return;
    }
    queue_write(Write::Timeline(rk, state, time, duration));
}

/// Block (briefly) until queued Plex writes are out; used when quitting.
fn flush_writes(limit: Duration) {
    let t0 = Instant::now();
    while PENDING.load(std::sync::atomic::Ordering::SeqCst) > 0 && t0.elapsed() < limit {
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn fetch_more(key: String) -> tokio::sync::oneshot::Receiver<anyhow::Result<Vec<Track>>> {
    plex_call(move |c| async move {
        match key.strip_prefix("query:") {
            Some(filter) => {
                let section = crate::section();
                c.random_tracks(&section, filter, 100).await
            }
            None => c.play_queue(&key).await.map(|(_, t)| t),
        }
    })
}

/// The app was called Aulos; move its caches once so nothing re-downloads.
fn migrate_old_caches() {
    let home = std::env::var("HOME").unwrap_or_default();
    let xdg = std::env::var("XDG_CACHE_HOME").unwrap_or_else(|_| format!("{home}/.cache"));
    for (old, new) in [(format!("{xdg}/aulos"), format!("{xdg}/crate")), ("/scratch/aulos-cache".into(), "/scratch/crate-cache".into())] {
        let (old, new) = (std::path::Path::new(&old), std::path::Path::new(&new));
        if old.is_dir() && !new.exists() {
            match std::fs::rename(old, new) {
                Ok(()) => eprintln!("[crate] moved {} -> {}", old.display(), new.display()),
                Err(e) => eprintln!("[crate] couldn't move {}: {e}", old.display()),
            }
        }
    }
}

fn main() {
    migrate_old_caches();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info,zbus=warn")).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--sign-out") {
        match account::remove() {
            Ok(()) if account::from_env().is_some() => {
                println!("Removed the saved sign-in, but PLEX_TOKEN/PLEX_URL/PLEX_SECTION in this environment still sign Crate in.")
            }
            Ok(()) => println!("Signed out: Crate will show its sign-in screen next launch."),
            Err(e) => eprintln!("couldn't remove the account: {e}"),
        }
        return;
    }
    if args.iter().any(|a| a == "--import-env") {
        // Save PLEX_URL / PLEX_TOKEN / PLEX_SECTION from the environment as
        // Crate's own sign-in (the token is never printed).
        match account::from_env() {
            Some(a) => match account::save(&a) {
                Ok(()) => println!("Saved the Plex sign-in for {} (library {}).", a.url, a.section),
                Err(e) => eprintln!("couldn't save: {e:#}"),
            },
            None => eprintln!("--import-env needs PLEX_TOKEN, PLEX_URL and PLEX_SECTION in the environment"),
        }
        return;
    }
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    if args.iter().any(|a| a == "--check-servers") {
        // Diagnostic: what sign-in would pick for the plex.tv token in PLEX_TOKEN (not printed).
        let Some(token) = std::env::var("PLEX_TOKEN").ok().filter(|t| !t.is_empty()) else {
            eprintln!("--check-servers needs PLEX_TOKEN");
            return;
        };
        rt.block_on(async {
            let http = reqwest::Client::new();
            match account::servers(&http, &token).await {
                Ok(list) => {
                    for s in list {
                        match account::pick_connection(&http, &s).await {
                            Ok((url, sections)) => println!("{}: {url} music libraries {:?}", s.name, sections),
                            Err(e) => println!("{}: {e:#}", s.name),
                        }
                    }
                }
                Err(e) => eprintln!("servers: {e:#}"),
            }
        });
        return;
    }
    if args.iter().any(|a| a == "--refresh-server") {
        // Fill in the stored account's server id and addresses from plex.tv
        // (needed for finding the server again on another network).
        let Some(mut a) = account::stored() else {
            eprintln!("no saved sign-in");
            return;
        };
        rt.block_on(async {
            let http = reqwest::Client::new();
            match account::servers(&http, &a.token).await {
                Ok(list) => {
                    // The saved machine id, else an exact name, else the only
                    // server this token belongs to; anything ambiguous is refused.
                    // The saved machine id, else the id the saved URL reports,
                    // else the only server this token belongs to; never a name.
                    let probed: Option<String> = async {
                        let v: serde_json::Value =
                            http.get(format!("{}/identity", a.url)).header("Accept", "application/json").send().await.ok()?.json().await.ok()?;
                        v["MediaContainer"]["machineIdentifier"].as_str().map(String::from)
                    }
                    .await;
                    let want = if a.server_id.is_empty() { probed } else { Some(a.server_id.clone()) };
                    let by_token: Vec<_> = list.iter().filter(|s| s.token == a.token).collect();
                    let pick = match want {
                        Some(id) => list.iter().find(|s| s.id == id),
                        None if by_token.len() == 1 => Some(by_token[0]),
                        None => None,
                    };
                    match pick {
                        Some(s) => {
                            a.remember(s);
                            match account::save(&a) {
                                Ok(()) => println!("Saved {} addresses for {}.", a.connections.len(), a.server),
                                Err(e) => eprintln!("couldn't save: {e:#}"),
                            }
                        }
                        None => eprintln!("can't tell which server {} is (no saved id, name or unique token match)", a.url),
                    }
                }
                Err(e) => eprintln!("servers: {e:#}"),
            }
        });
        return;
    }
    let Some(acct) = account::load() else {
        run_setup(rt);
        return;
    };
    // One Crate at a time: a second launch raises the first and exits.
    if !instance::acquire() {
        instance::hand_off();
        return;
    }
    let (transport_tx, transport_rx) = mpsc::unbounded_channel::<Transport>();
    instance::listen(transport_tx.clone());
    // Offline files are scoped by the address the account was saved with,
    // not whichever one answers this session.
    let saved_url = acct.url.clone();
    let acct = rt.block_on(account::ensure_reachable(acct));
    let (base, token, section) = (acct.url.clone(), acct.token.clone(), acct.section.clone());
    offline::set_scope(&acct.server_id, &saved_url);
    // Remote when the address in use is one Plex lists as not local (or a
    // relay). An address it doesn't list (PLEX_URL) counts as local.
    // CRATE_REMOTE=1/0 overrides, for testing.
    let remote = match std::env::var("CRATE_REMOTE").ok().as_deref() {
        Some("1") => true,
        Some("0") => false,
        _ => acct.connections.iter().find(|c| c.uri == acct.url).is_some_and(|c| !c.local || c.relay),
    };
    let _ = REMOTE.set(remote);
    let _ = SECTION.set(section.clone());

    // tokio lives on its own threads; GPUI never sees it except via channels.
    let (art_tx, art_rx_req) = mpsc::unbounded_channel::<ArtRequest>();
    let (art_res_tx, art_rx) = mpsc::unbounded_channel::<ArtResult>();
    let (albums_tx, albums_rx) = mpsc::unbounded_channel();
    let (tracks_tx, tracks_rx) = mpsc::unbounded_channel::<TracksRequest>();
    let plex_creds = (base.clone(), token.clone());

    let (search_tx, search_rx) = mpsc::unbounded_channel::<SearchRequest>();
    let (hubs_tx, hubs_rx) = mpsc::unbounded_channel::<HubsRequest>();
    let (artist_tx, artist_rx) = mpsc::unbounded_channel::<ArtistRequest>();
    let mpris_tx = mpris::start(transport_tx);
    // MPRIS starts at 1.0; tell it the saved level.
    let _ = mpris_tx.send(MprisUpdate::Volume(theme::saved_volume() as f64));
    let client = plex::Client::new(base, token);
    let _ = PLEX.set((rt.handle().clone(), client.clone()));
    std::thread::Builder::new()
        .name("tokio".into())
        .spawn(move || {
            rt.block_on(plex::run(client, section, albums_tx, art_rx_req, art_res_tx, tracks_rx, search_rx, hubs_rx, artist_rx));
        })
        .unwrap();

    gpui_platform::application().with_assets(icons::Icons).run(move |cx: &mut App| {
        gpui_component::init(cx);
        // Closing the window quits (so the on-quit report goes out).
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        load_fonts(cx);
        cx.bind_keys([
            gpui::KeyBinding::new("space", PlayPause, Some("Crate && !Input")),
            gpui::KeyBinding::new("n", NextTrack, Some("Crate && !Input")),
            gpui::KeyBinding::new("p", PrevTrack, Some("Crate && !Input")),
            gpui::KeyBinding::new("right", SeekForward, Some("Crate && !Input")),
            gpui::KeyBinding::new("left", SeekBack, Some("Crate && !Input")),
            gpui::KeyBinding::new("escape", Back, Some("Crate && !Input")),
            gpui::KeyBinding::new("f", ToggleNowPlaying, Some("Crate && !Input")),
            gpui::KeyBinding::new("g h", GoHome, Some("Crate && !Input")),
            gpui::KeyBinding::new("g s", GoStations, Some("Crate && !Input")),
            gpui::KeyBinding::new("l", ShowLyrics, Some("Crate && !Input")),
            gpui::KeyBinding::new("=", VolumeUp, Some("Crate && !Input")),
            gpui::KeyBinding::new("-", VolumeDown, Some("Crate && !Input")),
            gpui::KeyBinding::new("q", ShowQueue, Some("Crate && !Input")),
            gpui::KeyBinding::new("j", CursorNext, Some("Crate && !Input")),
            gpui::KeyBinding::new("k", CursorPrev, Some("Crate && !Input")),
            gpui::KeyBinding::new("enter", ActivateCursor, Some("Crate && !Input")),
            gpui::KeyBinding::new("x", MarkCursor, Some("Crate && !Input")),
            gpui::KeyBinding::new("r", RadioFromSelection, Some("Crate && !Input")),
            gpui::KeyBinding::new("1", Rate1, Some("Crate && !Input")),
            gpui::KeyBinding::new("2", Rate2, Some("Crate && !Input")),
            gpui::KeyBinding::new("3", Rate3, Some("Crate && !Input")),
            gpui::KeyBinding::new("4", Rate4, Some("Crate && !Input")),
            gpui::KeyBinding::new("5", Rate5, Some("Crate && !Input")),
            // gpui-component binds tab to IndentInline in its Input; bound later at
            // the same depth, this wins inside the palette's search box.
            gpui::KeyBinding::new("tab", PaletteRadio, Some("Palette > Input")),
            gpui::KeyBinding::new("ctrl-k", FocusSearch, Some("Crate")),
        ]);
        let bounds = Bounds::centered(None, size(px(1440.0), px(900.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                app_id: Some("crate".into()),
                titlebar: Some(gpui::TitlebarOptions { title: Some("Crate".into()), ..Default::default() }),
                ..Default::default()
            },
            move |window, cx| {
                let view = cx.new(|cx| Crate::new(window, cx, art_tx, art_rx, albums_rx, tracks_tx, plex_creds, transport_rx, mpris_tx, search_tx, hubs_tx, artist_tx));
                cx.new(|cx| gpui_component::Root::new(view, window, cx))
            },
        )
        .unwrap();
        cx.activate(true);
    });
}

#[allow(dead_code)]
fn _unused(_: Pixels) {}

#[cfg(test)]
mod transcode_tests {
    #[test]
    fn label_kbps() {
        assert_eq!(super::transcoded_kbps("plex:part-46213.tx128.mp3"), Some(128));
        assert_eq!(super::transcoded_kbps("plex:part-46213.flac"), None);
        assert_eq!(super::transcoded_kbps("plex:part-46213.mp3"), None);
        assert_eq!(super::transcoded_kbps("song.txt.mp3"), None);
    }
}
