//! The player bar's track visualizer. Omarchy-style: a left click cycles
//! the style, a right click cycles the colors, and press-and-drag seeks.
//! Styles: Plex's sonic profile of the track, or live views of what is
//! playing (spectrum, mirrored waveform, level meter), or a plain line.
//! Colors come from the current theme, so they follow Omarchy themes.

use std::{cell::Cell, rc::Rc};

use gpui::{Bounds, Context, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, div, prelude::*, px, rgb};

use crate::{Crate, mpris::Transport, theme::Theme};

/// Bars across the track bar (and spectrum bands).
pub(crate) const BARS: usize = 96;
/// FFT size for the spectrum (the whole output tap: finer bass bands).
const FFT: usize = crate_audio::audio::SCOPE_FRAMES;
/// Spectrum range: lowest band edge (Hz); the top is 18 kHz or Nyquist.
const LO_HZ: f32 = 40.0;
/// Pointer travel (px) that turns a click into a drag.
const DRAG_PX: f32 = 4.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum Style {
    #[default]
    Sonic,
    Spectrum,
    Wave,
    Meter,
    Line,
}

impl Style {
    pub const ALL: [Style; 5] = [Style::Sonic, Style::Spectrum, Style::Wave, Style::Meter, Style::Line];

    pub fn key(self) -> &'static str {
        match self {
            Style::Sonic => "sonic",
            Style::Spectrum => "spectrum",
            Style::Wave => "wave",
            Style::Meter => "meter",
            Style::Line => "line",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Style::Sonic => "Sonic profile",
            Style::Spectrum => "Spectrum",
            Style::Wave => "Waveform",
            Style::Meter => "Level meter",
            Style::Line => "Minimal line",
        }
    }

    /// Animates from the live output.
    pub fn live(self) -> bool {
        matches!(self, Style::Spectrum | Style::Wave | Style::Meter)
    }

    fn next(self) -> Style {
        let i = Style::ALL.iter().position(|s| *s == self).unwrap_or(0);
        Style::ALL[(i + 1) % Style::ALL.len()]
    }
}

/// Color schemes, all built from the current theme's palette.
pub(crate) const PALETTES: [&str; 5] = ["Accent", "Rainbow", "Warm", "Cool", "Mono"];

fn stops(p: usize, t: &Theme) -> Vec<u32> {
    match p % PALETTES.len() {
        0 => vec![t.accent, t.accent],
        1 => vec![t.blue, t.cyan, t.green, t.yellow, t.orange, t.red],
        2 => vec![t.yellow, t.orange, t.red, t.magenta],
        3 => vec![t.teal, t.cyan, t.blue, t.magenta],
        _ => vec![t.text_strong, t.text_strong],
    }
}

/// The scheme's color at `f` (0..1).
fn gradient(stops: &[u32], f: f32) -> u32 {
    if stops.len() < 2 {
        return stops.first().copied().unwrap_or(0xffffff);
    }
    let x = f.clamp(0.0, 1.0) * (stops.len() - 1) as f32;
    let i = (x.floor() as usize).min(stops.len() - 2);
    crate::theme::mix(stops[i], stops[i + 1], x - i as f32)
}

/// Visualizer state: style and colors, live-view smoothing, and the drag.
#[derive(Default)]
pub(crate) struct Viz {
    pub style: Style,
    pub palette: usize,
    buf: Vec<f32>,
    spectrum: Vec<f32>,
    level: [f32; 2],
    peak: [f32; 2],
    peak_at: [Option<std::time::Instant>; 2],
    /// The bar a left press started on and the pointer x there, and the
    /// drag position (0..1) once it moves.
    press: Option<(&'static str, f32)>,
    pub drag: Option<f32>,
    /// Each bar's bounds by id (Now Playing draws over the player bar).
    bounds: Rc<std::cell::RefCell<std::collections::HashMap<&'static str, Bounds<Pixels>>>>,
    /// When a track bar was last drawn: no bar on screen, no animation.
    drawn: Cell<Option<std::time::Instant>>,
    /// Last live sample: when, and the tap's head then.
    sampled: Option<(std::time::Instant, u64)>,
    /// When the tap's head last moved (new audio played).
    head_moved: Option<std::time::Instant>,
    /// A right press happened during this drag: on Wayland its release then
    /// reports no button held though the left still is, so don't take a
    /// buttonless move as a lost release.
    right_in_press: bool,
    /// Typical time between the tap's updates (one audio callback), smoothed:
    /// a gap longer than this is silence, shorter is just between callbacks.
    cb_gap: Option<std::time::Duration>,
}

impl Viz {
    pub fn load() -> Self {
        let (style, palette) = crate::theme::saved_viz();
        Self { style: Style::ALL.into_iter().find(|s| s.key() == style).unwrap_or_default(), palette, ..Default::default() }
    }

    /// Nothing left to fall: the live view shows silence.
    fn at_rest(&self) -> bool {
        // Only what the current style draws (and decays).
        self.buf.is_empty()
            && match self.style {
                // A NaN counts as rest, not as forever moving.
                Style::Spectrum => self.spectrum.iter().all(|&x| x.is_nan() || x < 0.005),
                Style::Meter => self.level.iter().chain(&self.peak).all(|&x| x.is_nan() || x < 0.005),
                _ => true,
            }
    }

    fn frac_at(&self, x: Pixels) -> Option<f32> {
        let (id, _) = self.press?;
        let b = *self.bounds.borrow().get(id)?;
        let w: f32 = b.size.width.into();
        (w > 0.0).then(|| ((f32::from(x) - f32::from(b.origin.x)) / w).clamp(0.0, 1.0))
    }

    /// Refresh the live view from the engine's output tap.
    /// With no engine (stopped), only falls to rest.
    fn sample(&mut self, scope: Option<&crate_audio::audio::Scope>) {
        // Smoothing is tuned per ~30 fps step: extra repaints (a drag,
        // hover) reuse the last frame instead of speeding it up.
        let now = std::time::Instant::now();
        let head = scope.map(|s| s.head()).unwrap_or(0);
        if self.sampled.is_some_and(|(t, _)| now.duration_since(t) < std::time::Duration::from_millis(30)) {
            return;
        }
        let fresh = scope.is_some() && self.sampled.is_none_or(|(_, h)| h != head);
        self.sampled = Some((now, head));
        // Hold the last frame through a normal gap between callbacks (they
        // can be long when another app forces a big buffer on the device).
        let hold = self.cb_gap.map(|g| g.mul_f32(1.6)).unwrap_or_default().clamp(std::time::Duration::from_millis(120), std::time::Duration::from_millis(700));
        if fresh {
            if let Some(prev) = self.head_moved {
                let gap = now.duration_since(prev);
                // Only gaps that look like callback spacing, not a pause.
                if gap < std::time::Duration::from_millis(1000) {
                    self.cb_gap = Some(match self.cb_gap {
                        Some(g) => g.mul_f32(0.8) + gap.mul_f32(0.2),
                        None => gap,
                    });
                }
            }
            self.head_moved = Some(now);
        } else if scope.is_some() && self.head_moved.is_some_and(|t| now.duration_since(t) < hold) {
            return;
        }
        // Nothing new played for a while (pause, stop, underrun): fall to rest.
        let have = fresh && scope.is_some_and(|s| s.latest(FFT, &mut self.buf));
        if !have {
            self.buf.clear();
        }
        match self.style {
            Style::Spectrum => {
                let rate = scope.map(|s| s.rate.load(std::sync::atomic::Ordering::Relaxed)).unwrap_or(44_100).max(8000) as f32;
                let bands = if have && self.buf.len() >= FFT * 2 { spectrum(&self.buf, rate) } else { vec![0.0; BARS] };
                if self.spectrum.len() != BARS {
                    self.spectrum = vec![0.0; BARS];
                }
                // Rise at once, fall off gently.
                for (s, b) in self.spectrum.iter_mut().zip(bands) {
                    *s = if b > *s { b } else { *s * 0.82 + b * 0.18 };
                }
            }
            Style::Meter => {
                let now = std::time::Instant::now();
                if self.buf.is_empty() {
                    for ch in 0..2 {
                        self.level[ch] *= 0.85;
                        self.peak[ch] = (self.peak[ch] - 0.02).max(0.0);
                    }
                    return;
                }
                let tail = self.buf.len().saturating_sub(2048 * 2);
                for ch in 0..2 {
                    let xs: Vec<f32> = self.buf[tail..].iter().skip(ch).step_by(2).copied().collect();
                    let rms = if xs.is_empty() { 0.0 } else { (xs.iter().map(|x| x * x).sum::<f32>() / xs.len() as f32).sqrt() };
                    let pk = xs.iter().fold(0.0f32, |m, x| m.max(x.abs()));
                    let lv = db_frac(rms, 54.0);
                    self.level[ch] = if lv > self.level[ch] { lv } else { self.level[ch] * 0.85 + lv * 0.15 };
                    let p = db_frac(pk, 54.0);
                    let held = self.peak_at[ch].is_some_and(|t| now.duration_since(t).as_millis() < 1200);
                    if p >= self.peak[ch] || !held {
                        if p >= self.peak[ch] {
                            self.peak_at[ch] = Some(now);
                        }
                        self.peak[ch] = if p >= self.peak[ch] { p } else { (self.peak[ch] - 0.02).max(p) };
                    }
                }
            }
            _ => {}
        }
    }
}

/// Map a linear amplitude to 0..1 over the top `range` dB.
fn db_frac(a: f32, range: f32) -> f32 {
    if a <= 0.0 {
        return 0.0;
    }
    ((20.0 * a.log10() + range) / range).clamp(0.0, 1.0)
}

/// Log-spaced band levels (0..1) of the latest FFT frames (interleaved stereo in).
fn spectrum(stereo: &[f32], rate: f32) -> Vec<f32> {
    let n = FFT;
    let start = stereo.len() - n * 2;
    let mut re: Vec<f32> = (0..n)
        .map(|i| {
            let m = 0.5 * (stereo[start + 2 * i] + stereo[start + 2 * i + 1]);
            // Hann window.
            let w = 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / (n - 1) as f32).cos();
            m * w
        })
        .collect();
    let mut im = vec![0.0f32; n];
    fft(&mut re, &mut im);
    let mag: Vec<f32> = (0..n / 2).map(|k| (re[k] * re[k] + im[k] * im[k]).sqrt() / (n as f32 / 4.0)).collect();
    let (lo, hi) = (LO_HZ, (rate / 2.0).min(18_000.0));
    (0..BARS)
        .map(|b| {
            let f0 = lo * (hi / lo).powf(b as f32 / BARS as f32);
            let f1 = lo * (hi / lo).powf((b + 1) as f32 / BARS as f32);
            let (p0, p1) = (f0 / rate * n as f32, f1 / rate * n as f32);
            let m = if p1 - p0 < 1.0 {
                // Narrower than a bin (the bass): read the curve at the band's
                // centre, so neighbouring bars don't repeat one bin as steps.
                let c = ((p0 * p1).sqrt()).clamp(1.0, (n / 2 - 2) as f32);
                let (i, t) = (c as usize, c.fract());
                mag[i] * (1.0 - t) + mag[i + 1] * t
            } else {
                let k0 = (p0 as usize).clamp(1, n / 2 - 1);
                let k1 = (p1.ceil() as usize).clamp(k0 + 1, n / 2);
                mag[k0..k1].iter().fold(0.0f32, |a, &x| a.max(x))
            };
            // Tilt up the highs a little: music falls off with frequency.
            db_frac(m * (1.0 + b as f32 / BARS as f32 * 3.0), 54.0)
        })
        .collect()
}

/// In-place iterative radix-2 FFT (len must be a power of two).
fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -std::f32::consts::TAU / len as f32;
        let (wr, wi) = (ang.cos(), ang.sin());
        for start in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0f32, 0.0f32);
            for k in 0..len / 2 {
                let (a, b) = (start + k, start + k + len / 2);
                let (tr, ti) = (re[b] * cr - im[b] * ci, re[b] * ci + im[b] * cr);
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                (cr, ci) = (cr * wr - ci * wi, cr * wi + ci * wr);
            }
        }
        len <<= 1;
    }
}

impl Crate {
    pub(crate) fn cycle_viz_style(&mut self, cx: &mut Context<Self>) {
        let style = {
            let mut v = self.viz.borrow_mut();
            v.style = v.style.next();
            // The last style's values would otherwise sit frozen.
            v.spectrum.iter_mut().for_each(|x| *x = 0.0);
            v.level = Default::default();
            v.peak = Default::default();
            v.style
        };
        self.save_viz();
        self.say(format!("Track bar: {}", style.label()));
        cx.notify();
    }

    pub(crate) fn cycle_viz_palette(&mut self, cx: &mut Context<Self>) {
        let p = {
            let mut v = self.viz.borrow_mut();
            v.palette = (v.palette + 1) % PALETTES.len();
            v.palette
        };
        self.save_viz();
        self.say(format!("Track bar colors: {}", PALETTES[p]));
        cx.notify();
    }

    fn save_viz(&mut self) {
        let (s, p) = {
            let v = self.viz.borrow();
            (v.style.key(), v.palette)
        };
        if let Err(e) = crate::theme::save_viz(s, p) {
            self.status = format!("Couldn't save the track bar style: {e}").into();
        }
    }

    /// A live style is showing over audio that is playing, or still settling
    /// to rest after a pause or stop: worth animating.
    pub(crate) fn viz_animating(&self) -> bool {
        let v = self.viz.borrow();
        let playing = self.engine.is_some() && self.now.as_ref().is_some_and(|n| !n.paused && !n.finished);
        v.style.live() && v.drawn.get().is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(250)) && (playing || !v.at_rest())
    }

    /// Repaint ~30 times a second while a live style animates.
    pub(crate) fn start_viz_ticker(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(std::time::Duration::from_millis(33)).await;
                let alive = this
                    .update(cx, |this, cx| {
                        if this.viz_animating() {
                            cx.notify();
                        }
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        })
        .detach();
    }

    /// The track bar: visualizer plus click / right-click / drag handling.
    /// `frac` is playback progress, `len_ms` the track length.
    pub(crate) fn track_viz(&self, id: &'static str, height: f32, frac: f32, len_ms: u64, levels: Option<&Vec<f32>>, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = self.theme.clone();
        let t = &theme;
        let mut v = self.viz.borrow_mut();
        v.drawn.set(Some(std::time::Instant::now()));
        if v.style.live() {
            v.sample(self.engine.as_ref().map(|e| &e.stats.scope));
        }
        // A drag shows on the bar it started on.
        let shown = v.drag.filter(|_| v.press.is_some_and(|(p, _)| p == id)).unwrap_or(frac);
        let st = stops(v.palette, t);
        let played = |i: usize| (i as f32 + 0.5) / BARS as f32 <= shown;
        let bar = |h: f32, color: u32| div().flex_1().h(px(h.max(1.0))).rounded(px(1.0)).bg(rgb(color));
        let body: gpui::AnyElement = match v.style {
            Style::Sonic => div()
                .size_full()
                .flex()
                .items_center()
                .gap(px(1.5))
                .children((0..BARS).map(|i| {
                    let lv = levels.and_then(|l| l.get(i)).copied();
                    let h = lv.map(|x| 3.0 + (height - 3.0) * x).unwrap_or(4.0);
                    bar(h, if played(i) { gradient(&st, i as f32 / BARS as f32) } else { t.line })
                }))
                .into_any_element(),
            Style::Spectrum => div()
                .size_full()
                .flex()
                .items_end()
                .gap(px(1.5))
                .children((0..BARS).map(|i| {
                    let lv = v.spectrum.get(i).copied().unwrap_or(0.0);
                    bar(2.0 + (height - 4.0) * lv, gradient(&st, lv))
                }))
                .into_any_element(),
            Style::Wave => {
                // Peak per slice of the latest ~45 ms, mirrored round the middle.
                // The latest ~45 ms only.
                let skip = v.buf.len().saturating_sub(2048 * 2) / 2;
                let frames = v.buf.len() / 2 - skip;
                let wave = &v.buf[skip * 2..];
                let per = (frames / BARS).max(1);
                // Auto-gain: scale to the window's peak (never boosting
                // near-silence past a floor), so the shape is always readable.
                let top = (0..frames).map(|f| 0.5 * (wave[2 * f] + wave[2 * f + 1]).abs()).fold(0.0f32, f32::max).max(0.08);
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .gap(px(1.5))
                    .children((0..BARS).map(|i| {
                        let a = (i * per..((i + 1) * per).min(frames)).map(|f| 0.5 * (wave[2 * f] + wave[2 * f + 1]).abs()).fold(0.0f32, f32::max);
                        let lv = (a / top).min(1.0);
                        bar(2.0 + (height - 2.0) * lv, gradient(&st, i as f32 / BARS as f32))
                    }))
                    .into_any_element()
            }
            Style::Meter => {
                let row = |ch: usize| {
                    let (lv, pk) = (v.level[ch], v.peak[ch]);
                    div()
                        .relative()
                        .w_full()
                        .h(px((height - 4.0) / 2.0))
                        .rounded(px(2.0))
                        .bg(rgb(t.line))
                        .child(div().absolute().left_0().top_0().bottom_0().w(gpui::relative(lv)).rounded(px(2.0)).bg(rgb(gradient(&st, lv))))
                        .child(div().absolute().top_0().bottom_0().left(gpui::relative(pk)).w(px(2.0)).bg(rgb(gradient(&st, pk))))
                };
                div().size_full().flex().flex_col().justify_center().gap(px(4.0)).child(row(0)).child(row(1)).into_any_element()
            }
            Style::Line => div()
                .size_full()
                .flex()
                .items_center()
                .child(
                    div()
                        .relative()
                        .w_full()
                        .h(px(3.0))
                        .rounded(px(2.0))
                        .bg(rgb(t.line))
                        .child(div().absolute().left_0().top_0().bottom_0().w(gpui::relative(shown)).rounded(px(2.0)).bg(rgb(gradient(&st, shown)))),
                )
                .into_any_element(),
        };
        // Live styles show progress as a thin line underneath.
        let progress = v.style.live().then(|| {
            div()
                .absolute()
                .left_0()
                .right_0()
                .bottom(px(-5.0))
                .h(px(2.0))
                .rounded(px(1.0))
                .bg(rgb(t.line))
                .child(div().h_full().w(gpui::relative(shown)).rounded(px(1.0)).bg(rgb(gradient(&st, shown))))
        });
        let bounds = v.bounds.clone();
        drop(v);
        let weak = cx.entity().downgrade();
        div()
            .id(id)
            .relative()
            .flex_1()
            .h(px(height))
            .cursor_pointer()
            .child(
                gpui::canvas(
                    move |b, _, _| {
                        bounds.borrow_mut().insert(id, b);
                    },
                    move |_, _, window, _| {
                        // Follow the pointer anywhere in the window while this bar
                        // is pressed, so a drag that leaves the thin bar keeps
                        // tracking. Registered every paint: a press triggers no repaint.
                        {
                            // A new left press anywhere means any earlier one has
                            // ended, even if its release was never seen (let go
                            // outside the window). Capture runs before the bar's own
                            // mouse-down, which then records a fresh press.
                            let weak = weak.clone();
                            window.on_mouse_event(move |ev: &MouseDownEvent, phase, _, cx| {
                                if phase != gpui::DispatchPhase::Capture || ev.button != MouseButton::Left {
                                    return;
                                }
                                weak.update(cx, |this, _| {
                                    let mut v = this.viz.borrow_mut();
                                    if v.press.is_some() {
                                        v.press = None;
                                        v.drag = None;
                                        v.right_in_press = false;
                                    }
                                })
                                .ok();
                            });
                        }
                        {
                            let weak = weak.clone();
                            window.on_mouse_event(move |ev: &MouseMoveEvent, phase, _, cx| {
                                if phase != gpui::DispatchPhase::Bubble {
                                    return;
                                }
                                // Any button: Wayland reports only the last one
                                // pressed, so a right press mid-drag reads as
                                // Right while the left is still held.
                                let held = ev.pressed_button.is_some();
                                let none = ev.pressed_button.is_none();
                                weak.update(cx, |this, cx| {
                                    let mut v = this.viz.borrow_mut();
                                    // No button down but a press is still recorded: its
                                    // release was never seen (it landed where its bar
                                    // wasn't drawn any more). Any bar may forget it.
                                    // After a right press mid-drag, buttonless moves
                                        // are still the drag (see `right_in_press`).
                                    let held = held || (none && v.right_in_press && v.press.is_some());
                                    if !held {
                                        if none && v.press.is_some() {
                                            v.press = None;
                                            v.drag = None;
                                            cx.notify();
                                        }
                                        return;
                                    }
                                    let Some((p, x0)) = v.press else { return };
                                    if p != id {
                                        return;
                                    }
                                    if v.drag.is_some() || (f32::from(ev.position.x) - x0).abs() > DRAG_PX {
                                        v.drag = v.frac_at(ev.position.x);
                                        cx.notify();
                                    }
                                })
                                .ok();
                            });
                        }
                    },
                )
                .absolute()
                .size_full(),
            )
            .child(body)
            .children(progress)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                    let mut v = this.viz.borrow_mut();
                    v.press = Some((id, ev.position.x.into()));
                    v.right_in_press = false;
                    v.drag = None;
                    cx.notify();
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseUpEvent, _, cx| {
                    let (press, drag) = {
                        let mut v = this.viz.borrow_mut();
                        let drag = v.drag.take().or_else(|| v.drag_target(ev.position.x));
                        (v.press.take().filter(|(p, _)| *p == id), drag)
                    };
                    match (press, drag) {
                        (Some(_), Some(f)) if len_ms > 0 => this.transport(Transport::SetPosition((f as f64 * len_ms as f64) as u64), cx),
                        (Some(_), None) => this.cycle_viz_style(cx),
                        _ => {}
                    }
                    cx.notify();
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseUpEvent, _, cx| {
                    let drag = {
                        let mut v = this.viz.borrow_mut();
                        // Only the bar the press started on; where the pointer let
                        // go, if it travelled far enough.
                        if v.press.is_none_or(|(p, _)| p != id) {
                            return;
                        }
                        let drag = v.drag_target(ev.position.x).or(v.drag);
                        v.press = None;
                        v.drag = None;
                        drag
                    };
                    if let Some(f) = drag
                        && len_ms > 0
                    {
                        this.transport(Transport::SetPosition((f as f64 * len_ms as f64) as u64), cx);
                    }
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    // Not in the middle of a drag-to-seek.
                    let mut v = this.viz.borrow_mut();
                    if v.press.is_some() {
                        v.right_in_press = true;
                    } else {
                        drop(v);
                        this.cycle_viz_palette(cx);
                    }
                }),
            )
            .into_any_element()
    }
}

impl Viz {
    /// A release that moved past the drag threshold without a move event
    /// in between still seeks to where it ended.
    fn drag_target(&self, x: Pixels) -> Option<f32> {
        let (_, x0) = self.press?;
        ((f32::from(x) - x0).abs() > DRAG_PX).then(|| self.frac_at(x)).flatten()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn bass_bars_are_not_steps() {
        // A 70 Hz tone at 44.1 kHz: bars around it rise then fall with no run
        // of identical neighbours (one bin repeated).
        let rate = 44_100.0;
        let stereo: Vec<f32> = (0..super::FFT).flat_map(|i| {
            let v = 0.5 * (std::f32::consts::TAU * 70.0 * i as f32 / rate).sin();
            [v, v]
        }).collect();
        let bands = super::spectrum(&stereo, rate);
        let low = &bands[..20];
        let repeats = low.windows(2).filter(|w| (w[0] - w[1]).abs() < 1e-6 && w[0] > 0.0).count();
        assert_eq!(repeats, 0, "{low:?}");
        let peak = low.iter().cloned().fold(0.0f32, f32::max);
        assert!(peak > 0.8, "{low:?}");
    }

    #[test]
    fn fft_finds_a_tone() {
        // 1 kHz at 48 kHz: the spectrum's loudest band contains 1 kHz.
        let n = super::FFT;
        let stereo: Vec<f32> = (0..n).flat_map(|i| {
            let s = (std::f32::consts::TAU * 1000.0 * i as f32 / 48_000.0).sin() * 0.5;
            [s, s]
        }).collect();
        let bands = super::spectrum(&stereo, 48_000.0);
        let top = bands.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap().0;
        let lo = super::LO_HZ;
        let f0 = lo * (18_000.0f32 / lo).powf(top as f32 / super::BARS as f32);
        let f1 = lo * (18_000.0f32 / lo).powf((top + 1) as f32 / super::BARS as f32);
        assert!(f0 <= 1050.0 && f1 >= 950.0, "loudest band {f0}..{f1} Hz");
        assert!(bands[top] > 0.8);
        assert_eq!(super::gradient(&[0x000000, 0xffffff], 0.0), 0x000000);
        assert_eq!(super::gradient(&[0x000000, 0xffffff], 1.0), 0xffffff);
    }
}

