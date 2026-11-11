//! Compact layout from the design canvas (Tile.dc.html): what Crate shows in
//! a narrow tiled window. Playing tab plus a four-item bottom nav.

use gpui::{Context, FontWeight, Hsla, ObjectFit, div, img, linear_color_stop, linear_gradient, prelude::*, px, rgb};

use crate::{Crate, View, fmt_ms, icons, mpris::Transport, shell::MONO, theme::{Theme, mix}};

fn white(a: f32) -> Hsla {
    gpui::hsla(0.0, 0.0, 1.0, a)
}

impl Crate {
    pub(crate) fn tile_view(&self, t: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(n) = self.now.as_ref() else { return div().into_any_element() };
        let tr = &n.tracks[n.idx];
        let detail = self.track_detail.get(&tr.rating_key).cloned().flatten();
        let blur = self.album_detail.get(&tr.album_key).cloned().flatten().and_then(|d| d.blur);
        let top = blur.map(|b| mix(mix(b[0], b[1], 0.5), t.base, 0.4)).unwrap_or(mix(t.base, t.discovery, 0.25));
        let paused = n.paused || n.finished;
        let side = (self.content_w + 2.0 * crate::PAD_X - 80.0).clamp(160.0, 420.0);

        let cover = match self.art.get(&tr.thumb).cloned() {
            Some(i) => img(i).size(px(side)).rounded(px(10.0)).object_fit(ObjectFit::Cover).into_any_element(),
            None => gpui::img(self.mark(crate::art::placeholder_for(&tr.thumb))).size(px(side)).rounded(px(10.0)).object_fit(gpui::ObjectFit::Cover).into_any_element(),
        };
        let len = tr.duration_ms;
        let frac = if len > 0 { (n.elapsed_ms as f32 / len as f32).min(1.0) } else { 0.0 };
        let levels = detail.as_ref().map(|d| d.levels.clone()).filter(|l| !l.is_empty());
        let bars = self.track_viz("tile-bar", 28.0, frac, len, levels.as_ref(), cx);
        // While dragging to seek, the times show where the drop would land.
        let shown_ms = self.viz.borrow().drag.map(|f| (f as f64 * len as f64) as u64).unwrap_or(n.elapsed_ms);
        let format = n.formats.get(&n.idx).map(|&(rate, bits)| {
            if let Some(k) = n.transcoded.get(&n.idx) {
                return format!("Opus {k}k · transcoded");
            }
            let codec = detail.as_ref().map(|d| d.codec.clone()).unwrap_or_default();
            let khz = if rate % 1000 == 0 { format!("{}", rate / 1000) } else { format!("{:.1}", rate as f32 / 1000.0) };
            format!("{codec} {bits}/{khz}").trim().to_string()
        });
        let btn = |id: &'static str, ic: &'static str, tr: Transport| {
            div()
                .id(id)
                .size(px(44.0))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .child(gpui::svg().path(format!("icons/{ic}.svg")).size(px(24.0)).text_color(white(1.0)))
                .on_click(cx.listener(move |this, _, _, cx| this.transport(tr, cx)))
        };
        let queue: Vec<_> = n
            .tracks
            .iter()
            .enumerate()
            .skip(n.idx + 1)
            .take(6)
            .map(|(i, q)| {
                let art = match self.art.get(&q.thumb).cloned() {
                    Some(im) => img(im).size(px(36.0)).flex_none().rounded(px(4.0)).object_fit(ObjectFit::Cover).into_any_element(),
                    None => gpui::img(self.mark(crate::art::placeholder_for(&q.thumb))).size(px(36.0)).flex_none().rounded(px(4.0)).object_fit(gpui::ObjectFit::Cover).into_any_element(),
                };
                div()
                    .id(("tq", i))
                    .p(px(6.0))
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .rounded(px(8.0))
                    .cursor_pointer()
                    .hover(|d| d.bg(white(0.06)))
                    .child(art)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .child(div().text_size(px(13.0)).font_weight(FontWeight::MEDIUM).text_color(white(1.0)).truncate().child(q.title.clone()))
                            .child(div().text_size(px(12.0)).text_color(white(0.66)).truncate().child(q.artist.clone())),
                    )
                    .child(div().font_family(MONO).text_size(px(11.0)).text_color(white(0.66)).child(fmt_ms(q.duration_ms)))
                    .on_click(cx.listener(move |this, _, _, cx| this.play_from(i, 0, cx)))
            })
            .collect();

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(linear_gradient(180.0, linear_color_stop(rgb(top), 0.0), linear_color_stop(rgb(t.base), 0.75)))
            .child(
                div()
                    .px(px(18.0))
                    .pt(px(14.0))
                    .font_family(MONO)
                    .font_weight(FontWeight::MEDIUM)
                    .text_size(px(11.0))
                    .text_color(white(0.7))
                    .truncate()
                    .child(n.context.clone()),
            )
            .child(div().pt(px(10.0)).flex().justify_center().child(div().shadow_2xl().rounded(px(10.0)).child(cover)))
            .child(
                div()
                    .px(px(24.0))
                    .pt(px(18.0))
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(div().text_size(px(22.0)).font_weight(FontWeight::BOLD).text_color(white(1.0)).truncate().child(tr.title.clone()))
                    .child(div().text_size(px(14.0)).text_color(white(0.75)).truncate().child(tr.artist.clone())),
            )
            .child(
                div()
                    .px(px(24.0))
                    .pt(px(14.0))
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(div().flex().child(bars))
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .font_family(MONO)
                            .text_size(px(11.0))
                            .text_color(white(0.7))
                            .child(fmt_ms(shown_ms))
                            .child(format.unwrap_or_default())
                            .child(format!("−{}", fmt_ms(len.saturating_sub(shown_ms)))),
                    ),
            )
            .child(
                div()
                    .pt(px(10.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(px(22.0))
                    .child(btn("t-prev", "prev", Transport::Prev))
                    .child(
                        div()
                            .id("t-pp")
                            .size(px(60.0))
                            .rounded(px(30.0))
                            .bg(white(1.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .child(icons::icon(if paused { "play" } else { "pause" }, 24.0, t.base))
                            .on_click(cx.listener(|this, _, _, cx| this.transport(Transport::PlayPause, cx))),
                    )
                    .child(btn("t-next", "next", Transport::Next)),
            )
            .child(
                div()
                    .id("t-queue")
                    .flex_1()
                    .min_h_0()
                    .mt(px(16.0))
                    .mx(px(12.0))
                    .p(px(10.0))
                    .rounded(px(14.0))
                    .bg(gpui::hsla(240.0 / 360.0, 0.25, 0.06, 0.55))
                    .border_1()
                    .border_color(white(0.08))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(div().px(px(6.0)).pt(px(4.0)).pb(px(6.0)).font_family(MONO).font_weight(FontWeight::MEDIUM).text_size(px(11.0)).text_color(white(0.66)).child("UP NEXT"))
                    .children(queue),
            )
            .into_any_element()
    }

    /// Bottom nav for the compact layout: Playing, Home, Search, Library.
    pub(crate) fn compact_nav(&self, t: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let playing = self.compact_playing && self.now.is_some();
        let active = if playing {
            0
        } else {
            match self.view {
                View::Home => 1,
                View::Library | View::Album { .. } | View::Artist { .. } => 3,
                _ => 9,
            }
        };
        let items: [(&str, &str); 4] = [("Playing", "tracks"), ("Home", "home"), ("Search", "search"), ("Library", "albums")];
        div()
            .flex_none()
            .px(px(8.0))
            .pt(px(8.0))
            .pb(px(10.0))
            .flex()
            .bg(rgb(t.base))
            .children(items.into_iter().enumerate().map(|(i, (label, ic))| {
                let on = i == active;
                let c = if on { t.accent } else { mix(t.text, t.base, 0.3) };
                div()
                    .id(("cnav", i))
                    .flex_1()
                    .h(px(52.0))
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(4.0))
                    .cursor_pointer()
                    .child(gpui::svg().path(format!("icons/{ic}.svg")).size(px(20.0)).text_color(rgb(c)))
                    .child(div().text_size(px(11.0)).font_weight(FontWeight::MEDIUM).text_color(rgb(c)).child(label))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        match i {
                            0 => this.compact_playing = true,
                            1 => {
                                this.compact_playing = false;
                                if !matches!(this.view, View::Home) {
                                    this.go(View::Home);
                                }
                            }
                            2 => return this.open_palette(window, cx),
                            _ => {
                                this.compact_playing = false;
                                if !matches!(this.view, View::Library) {
                                    this.go(View::Library);
                                }
                            }
                        }
                        cx.notify();
                    }))
            }))
            .into_any_element()
    }
}
