//! Player bar from the design canvas (PlayerBar.dc.html).

use gpui::{Context, FontWeight, ObjectFit, SharedString, div, img, prelude::*, px, rgb};

use crate::{Aulos, fmt_ms, icons, mpris::Transport, shell::MONO, theme::Theme};

/// Seek bar segments (the canvas draws 96 bars).
pub(crate) const BARS: usize = 96;

fn khz(r: u32) -> String {
    if r.is_multiple_of(1000) { format!("{}", r / 1000) } else { format!("{:.1}", r as f32 / 1000.0) }
}

impl Aulos {
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
            None => div().size(px(56.0)).flex_none().rounded(px(6.0)).bg(rgb(t.raised)).into_any_element(),
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
            .child(icon_btn("shuffle", "shuffle", 17.0, t.line, None, cx))
            .child(icon_btn("prev", "prev", 20.0, t.text, Some(Transport::Prev), cx))
            .child(play)
            .child(icon_btn("next", "next", 20.0, t.text, Some(Transport::Next), cx))
            .child(icon_btn("repeat", "repeat", 17.0, t.line, None, cx));
        let levels = now.and_then(|n| self.track_detail.get(&n.tracks[n.idx].rating_key)).and_then(|d| d.as_ref()).map(|d| &d.levels).filter(|l| !l.is_empty());
        let bars: Vec<_> = (0..BARS)
            .map(|i| {
                let ms = len_ms * i as u64 / BARS as u64;
                let filled = (i as f32 + 0.5) / BARS as f32 <= frac;
                // Real loudness levels when the server has them, else a flat line.
                let h = levels.and_then(|l| l.get(i)).map(|&v| (3.0 + 19.0 * v).round()).unwrap_or(4.0);
                div()
                    .id(("seek", i))
                    .flex_1()
                    .h(px(22.0))
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .child(div().w_full().h(px(h)).rounded(px(1.0)).bg(rgb(if filled { t.accent } else { t.line })))
                    .when(len_ms > 0, |d| d.on_click(cx.listener(move |this, _, _, cx| this.transport(Transport::SetPosition(ms), cx))))
            })
            .collect();
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
                    .child(div().w(px(36.0)).flex().justify_end().child(fmt_ms(elapsed)))
                    .child(div().flex_1().flex().gap(px(1.5)).children(bars))
                    .child(div().w(px(36.0)).child(fmt_ms(len_ms))),
            );

        // Right: signal path badge, panels, output, volume.
        let badge = now.and_then(|n| {
            let &(rate, bits) = n.formats.get(&n.idx)?;
            let fmt = format!("{bits}/{}", khz(rate));
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
            .child(icon_btn("lyrics", "lyrics", 17.0, t.line, None, cx))
            .child(icon_btn("queue", "queue", 17.0, t.line, None, cx))
            .child(icon_btn("device", "device", 17.0, if self.target.is_some() { t.accent } else { t.text2 }, None, cx))
            .child(icons::icon("volume", 17.0, t.line))
            .child(div().w(px(72.0)).h(px(4.0)).rounded(px(2.0)).bg(rgb(t.border)).child(div().w_full().h_full().rounded(px(2.0)).bg(rgb(t.line))));

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
