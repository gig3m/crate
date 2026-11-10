//! Aulos GPUI spike: themed shell, virtualized album grid over a real Plex
//! library, cover art fetched on a tokio side thread, and a scroll benchmark.
//!
//! Env:
//!   PLEX_URL      (default http://plex-server:32400)
//!   PLEX_TOKEN    (required; set in the environment)
//!   PLEX_SECTION  (default 5)
//!   AULOS_ITEMS   (default 50000; real albums are cycled to fill)
//!   AULOS_BENCH   (set: auto-scroll, print frame stats, quit)

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

use plex::{Album, ArtRequest, ArtResult};
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
    fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        art_tx: mpsc::UnboundedSender<ArtRequest>,
        mut art_rx: mpsc::UnboundedReceiver<ArtResult>,
        albums_rx: tokio::sync::oneshot::Receiver<anyhow::Result<Vec<Album>>>,
    ) -> Self {
        let (theme, theme_gen) = Theme::load();
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search, play, or run a command"));

        // Albums arrive from the tokio side thread; awaited on GPUI's executor.
        cx.spawn(async move |this, cx| {
            let res = albums_rx.await;
            this.update(cx, |this, cx| {
                match res {
                    Ok(Ok(albums)) => {
                        this.status = format!("{} albums from Plex", albums.len()).into();
                        this.albums = albums;
                    }
                    Ok(Err(e)) => this.status = format!("Plex error: {e:#}").into(),
                    Err(_) => this.status = "Plex task dropped".into(),
                }
                cx.notify();
            })
            .ok();
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
            .unwrap_or(50_000);
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
        let max_y = (self.total.div_ceil(self.cols.max(1)) as f32) * ROW_H;
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
            if ix >= self.total {
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
                    .on_click(cx.listener(move |_, _, _, _| {
                        eprintln!("[aulos] clicked album #{ix}");
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
        let vw: f32 = window.viewport_size().width.into();
        let usable = (vw - SIDEBAR_W - PAD_X * 2.0).max(CARD_W);
        self.cols = (((usable + GAP) / (CARD_W + GAP)).floor() as usize).max(1);
        let rows = self.total.div_ceil(self.cols);

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
                let active = i == 1;
                div()
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
            .child(
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
                                "{} cells · {} · {} cols",
                                self.total, self.status, self.cols
                            )),
                    ),
            );

        let grid = uniform_list(
            "albums",
            rows,
            cx.processor(|this, range: std::ops::Range<usize>, _window, cx| {
                range.map(|r| this.row(r, cx)).collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.scroll)
        .flex_1();

        let player = div()
            .h(px(88.0))
            .flex_none()
            .bg(rgb(t.surface))
            .border_t_1()
            .border_color(rgb(t.border))
            .px(px(24.0))
            .flex()
            .items_center()
            .gap(px(16.0))
            .child(div().size(px(56.0)).rounded(px(6.0)).bg(rgb(t.raised)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(div().text_color(rgb(t.text_strong)).child("Nothing playing"))
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(rgb(t.muted))
                            .child("GPUI spike · no audio engine yet"),
                    ),
            )
            .child(div().flex_1())
            .child(
                div()
                    .w(px(480.0))
                    .h(px(4.0))
                    .rounded(px(2.0))
                    .bg(rgb(t.border))
                    .child(div().w(px(200.0)).h_full().rounded(px(2.0)).bg(rgb(t.accent))),
            )
            .child(div().flex_1());

        let el = div()
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
                            .child(grid),
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
    let (albums_tx, albums_rx) = tokio::sync::oneshot::channel();
    let client = plex::Client::new(base, token);
    std::thread::Builder::new()
        .name("tokio".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(plex::run(client, section, albums_tx, art_rx_req, art_res_tx));
        })
        .unwrap();

    gpui_platform::application().run(move |cx: &mut App| {
        gpui_component::init(cx);
        let bounds = Bounds::centered(None, size(px(1440.0), px(900.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                app_id: Some("aulos-spike".into()),
                ..Default::default()
            },
            move |window, cx| {
                let view = cx.new(|cx| Aulos::new(window, cx, art_tx, art_rx, albums_rx));
                cx.new(|cx| gpui_component::Root::new(view, window, cx))
            },
        )
        .unwrap();
        cx.activate(true);
    });
}

#[allow(dead_code)]
fn _unused(_: Pixels) {}
