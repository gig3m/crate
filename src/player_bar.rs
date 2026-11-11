//! Player bar from the design canvas (PlayerBar.dc.html).

use gpui::{Context, FontWeight, ObjectFit, SharedString, div, img, prelude::*, px, rgb};

use crate::{Crate, fmt_ms, icons, mpris::Transport, shell::MONO, theme::Theme};

/// Seek bar segments (the canvas draws 96 bars).
/// Sonic profile points per track (the visualizer's bar count).
pub(crate) const BARS: usize = crate::viz::BARS;

fn khz(r: u32) -> String {
    if r.is_multiple_of(1000) { format!("{}", r / 1000) } else { format!("{:.1}", r as f32 / 1000.0) }
}

impl Crate {
    pub(crate) fn player_bar(&self, t: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let now = self.now.as_ref();
        let track = now.map(|n| &n.tracks[n.idx]);
        let len_ms = track.map(|tr| tr.duration_ms).unwrap_or(0);
        let elapsed = now.map(|n| n.elapsed_ms).unwrap_or(0);
        let frac = if len_ms > 0 { (elapsed as f32 / len_ms as f32).min(1.0) } else { 0.0 };
        let paused = now.is_none_or(|n| n.paused || n.finished);

        // Left: cover, title, artist · album.
        let cover = match track.and_then(|tr| self.art.get(&tr.thumb).cloned()) {
            Some(i) => img(i).size(px(56.0)).flex_none().rounded(px(6.0)).object_fit(ObjectFit::Cover).into_any_element(),
            None if track.is_none() => div().size(px(56.0)).flex_none().rounded(px(6.0)).bg(rgb(t.raised)).into_any_element(),
            None => gpui::img(self.mark(crate::art::placeholder_for(track.map(|tr| tr.thumb.as_str()).unwrap_or("")))).size(px(56.0)).flex_none().rounded(px(6.0)).object_fit(gpui::ObjectFit::Cover).into_any_element(),
        };
        let (title, sub): (SharedString, SharedString) = match (now, track) {
            (Some(_), Some(tr)) => (tr.title.clone().into(), format!("{} · {}", tr.artist, tr.album_title).into()),
            _ => ("Nothing playing".into(), "Pick an album to start".into()),
        };
        let left = div()
            .id("np")
            .flex_shrink(1.0)
            .w(px(300.0))
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(14.0))
            .when(now.is_some(), |d| {
                d.cursor_pointer().on_click(cx.listener(|this, _, _, cx| {
                    this.now_playing = true;
                    cx.notify();
                }))
            })
            .child(cover)
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(div().text_size(px(14.0)).font_weight(FontWeight::SEMIBOLD).text_color(rgb(t.text_strong)).truncate().child(title))
                    .child(div().text_size(px(12.0)).text_color(rgb(t.text2)).truncate().child(sub)),
            );

        // Center: transport and seek bar.
        let icon_btn = |id: &'static str, ic: &'static str, size: f32, color: u32, tr: Option<Transport>, cx: &mut Context<Self>| {
            div()
                .id(id)
                .size(px(32.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.0))
                .child(icons::icon(ic, size, color))
                .when_some(tr, |d, tr| {
                    d.cursor_pointer().hover(|d| d.bg(rgb(t.raised))).on_click(cx.listener(move |this, _, _, cx| this.transport(tr, cx)))
                })
        };
        let play = div()
            .id("pp")
            .size(px(40.0))
            .rounded(px(20.0))
            .bg(rgb(t.text_strong))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|d| d.opacity(0.9))
            .child(icons::icon(if paused { "play" } else { "pause" }, 18.0, t.base))
            .on_click(cx.listener(|this, _, _, cx| this.transport(Transport::PlayPause, cx)));
        let controls = div()
            .flex()
            .items_center()
            .gap(px(18.0))
            .child(
                icon_btn("shuffle", "shuffle", 17.0, if self.shuffle { t.accent } else { t.text2 }, None, cx)
                    .cursor_pointer()
                    .hover(|d| d.bg(rgb(t.raised)))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_shuffle(cx))),
            )
            .child(icon_btn("prev", "prev", 20.0, t.text, Some(Transport::Prev), cx))
            .child(play)
            .child(icon_btn("next", "next", 20.0, t.text, Some(Transport::Next), cx))
            .child({
                let on = self.repeat != crate::queue::Repeat::Off;
                icon_btn("repeat", "repeat", 17.0, if on { t.accent } else { t.text2 }, None, cx)
                    .relative()
                    .cursor_pointer()
                    .hover(|d| d.bg(rgb(t.raised)))
                    .when(self.repeat == crate::queue::Repeat::One, |d| {
                        d.child(
                            div()
                                .absolute()
                                .top(px(3.0))
                                .right(px(3.0))
                                .font_family(MONO)
                                .font_weight(FontWeight::BOLD)
                                .text_size(px(9.0))
                                .text_color(rgb(t.accent))
                                .child("1"),
                        )
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.cycle_repeat(cx)))
            });
        // While dragging to seek, the time shows where the drop would land.
        let shown_ms = self.viz.borrow().drag.map(|f| (f as f64 * len_ms as f64) as u64).unwrap_or(elapsed);
        let levels = now.and_then(|n| self.track_detail.get(&n.tracks[n.idx].rating_key)).and_then(|d| d.as_ref()).map(|d| &d.levels).filter(|l| !l.is_empty());
        let center = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(6.0))
            .child(controls)
            .child(
                div()
                    .w_full()
                    .max_w(px(640.0))
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .font_family(MONO)
                    .text_size(px(11.0))
                    .text_color(rgb(t.muted))
                    .child(div().w(px(36.0)).flex().justify_end().child(fmt_ms(shown_ms)))
                    .child(self.track_viz("track-bar", 22.0, frac, len_ms, levels, cx))
                    .child(div().w(px(36.0)).child(fmt_ms(len_ms))),
            );

        // Right: signal path badge, panels, output, volume.
        let badge = now.and_then(|n| {
            let &(rate, bits) = n.formats.get(&n.idx)?;
            if let Some(k) = n.transcoded.get(&n.idx) {
                return Some((format!("Opus {k}k · transcoded"), t.muted, false));
            }
            // Lossy formats have no bit depth: name the codec instead.
            let fmt = if bits == 0 {
                let ext = n.tracks.get(n.idx).and_then(|t| t.part_key.rsplit('.').next().map(str::to_lowercase)).unwrap_or_default();
                let codec = match ext.as_str() {
                    "m4a" | "mp4" | "aac" => "AAC",
                    "mp3" => "MP3",
                    "ogg" | "opus" => "Opus",
                    _ => "lossy",
                };
                format!("{codec} {}", khz(rate))
            } else {
                format!("{bits}/{}", khz(rate))
            };
            Some(if n.graph_rate == 0 {
                (fmt, t.muted, false)
            } else if n.graph_rate == rate {
                (format!("{fmt} · native rate"), t.healthy, true)
            } else {
                (format!("{fmt} → {} · resampled", khz(n.graph_rate)), t.muted, false)
            })
        });
        let right = div()
            .flex_shrink(1.0)
            .w(px(300.0))
            .min_w_0()
            .flex()
            .items_center()
            .justify_end()
            .gap(px(6.0))
            .children(badge.map(|(text, color, good)| {
                div()
                    .px(px(6.0))
                    .py(px(3.0))
                    .rounded(px(4.0))
                    .border_1()
                    .border_color(rgb(if good { crate::theme::mix(t.deep, color, 0.25) } else { t.line }))
                    .bg(rgb(if good { crate::theme::mix(t.deep, color, 0.08) } else { t.deep }))
                    .font_family(MONO)
                    .font_weight(FontWeight::MEDIUM)
                    .text_size(px(10.0))
                    .text_color(rgb(color))
                    .whitespace_nowrap()
                    .child(text)
            }))
            .child(
                icon_btn("lyrics", "lyrics", 17.0, t.text2, None, cx)
                    .cursor_pointer()
                    .hover(|d| d.bg(rgb(t.raised)))
                    .on_click(cx.listener(|this, _, _, cx| this.open_now_playing(None, cx))),
            )
            .child(
                icon_btn("queue", "queue", 17.0, t.text2, None, cx)
                    .cursor_pointer()
                    .hover(|d| d.bg(rgb(t.raised)))
                    .on_click(cx.listener(|this, _, _, cx| this.open_queue(cx))),
            )
            .child(
                icon_btn("device", "device", 17.0, if self.target.is_some() { t.accent } else { t.text2 }, None, cx)
                    .cursor_pointer()
                    .hover(|d| d.bg(rgb(t.raised)))
                    .on_click(cx.listener(|this, _, _, cx| this.open_audio_settings(cx))),
            )
            .child(
                div()
                    .id("mute")
                    .size(px(28.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.0))
                    .cursor_pointer()
                    .hover(|d| d.bg(rgb(t.raised)))
                    .child(icons::icon(if self.volume == 0.0 { "volume-off" } else { "volume" }, 17.0, t.text2))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_mute(cx))),
            )
            .child({
                // 20 click targets across the bar; scroll nudges by 5%.
                const STEPS: usize = 20;
                let v = self.volume;
                div()
                    .id("volume")
                    .w(px(84.0))
                    .h(px(16.0))
                    .flex()
                    .items_center()
                    .relative()
                    .on_scroll(cx.listener(|this, ev: &gpui::ScrollEvent, _, cx| {
                        // One 5% step per ~40 px of scroll (a mouse notch is 20 px
                        // here; trackpads send many tiny deltas).
                        let dy: f32 = ev.delta.pixel_delta(px(40.0)).y.into();
                        this.volume_scroll += dy;
                        let steps = (this.volume_scroll / 40.0).trunc();
                        if steps != 0.0 {
                            this.volume_scroll -= steps * 40.0;
                            this.set_volume(this.volume + 0.05 * steps, cx);
                        }
                    }))
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .right_0()
                            .h(px(4.0))
                            .rounded(px(2.0))
                            .bg(rgb(t.border))
                            .child(div().h_full().rounded(px(2.0)).w(gpui::relative(v)).bg(rgb(if v < 1.0 { t.text } else { t.healthy }))),
                    )
                    .child(div().absolute().inset_0().flex().children((0..STEPS).map(|i| {
                        let to = (i + 1) as f32 / STEPS as f32;
                        div()
                            .id(("vol", i))
                            .flex_1()
                            .h_full()
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| this.set_volume(to, cx)))
                    })))
            });

        div()
            .h(px(88.0))
            .flex_none()
            .bg(rgb(t.deep))
            .border_t_1()
            .border_color(rgb(t.raised))
            .px(px(20.0))
            .flex()
            .items_center()
            .gap(px(24.0))
            .child(left)
            .child(center)
            .child(right)
            .into_any_element()
    }
}
