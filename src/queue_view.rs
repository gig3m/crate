//! The full queue: what's played (click to play again), what's playing, and
//! what's next, which can be reordered (drag a row), removed or cleared,
//! plus "stop after this track / album" and Save as playlist. Tracks the
//! engine has already started loading can't be moved or removed.

use gpui::{Context, FontWeight, ObjectFit, div, img, prelude::*, px, rgb, uniform_list};

use crate::{Crate, PAD_X, View, fmt_ms, icons, shell::MONO, theme::Theme};

/// A row being dragged to a new place in the upcoming tracks.
#[derive(Clone)]
pub(crate) struct QueueDrag {
    from: usize,
    /// The dragged track, found again on drop (the queue may have shifted).
    key: String,
    queue_id: u64,
}

pub(crate) struct QueueGhost {
    title: String,
    bg: u32,
    border: u32,
    text: u32,
}

impl gpui::Render for QueueGhost {
    fn render(&mut self, _: &mut gpui::Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px(px(12.0))
            .py(px(8.0))
            .rounded(px(8.0))
            .bg(rgb(self.bg))
            .border_1()
            .border_color(rgb(self.border))
            .text_size(px(13.0))
            .text_color(rgb(self.text))
            .child(self.title.clone())
    }
}

/// One line of the list: a section heading or a queue index.
#[derive(Clone, Copy)]
enum Entry {
    Head(&'static str),
    Track(usize),
}

fn entries(idx: usize, len: usize) -> Vec<Entry> {
    let mut v = Vec::with_capacity(len + 3);
    if idx > 0 {
        v.push(Entry::Head("PLAYED"));
        v.extend((0..idx).map(Entry::Track));
    }
    v.push(Entry::Head("NOW PLAYING"));
    v.push(Entry::Track(idx));
    if idx + 1 < len {
        v.push(Entry::Head("NEXT UP"));
        v.extend((idx + 1..len).map(Entry::Track));
    }
    v
}

impl Crate {
    /// Open the queue page, scrolled to what's playing.
    pub(crate) fn open_queue(&mut self, cx: &mut Context<Self>) {
        self.now_playing = false;
        if !matches!(self.view, View::Soon("queue")) {
            self.go(View::Soon("queue"));
        }
        if let Some(n) = self.now.as_ref() {
            let at = entries(n.idx, n.tracks.len()).iter().position(|e| matches!(e, Entry::Track(i) if *i == n.idx)).unwrap_or(0);
            self.queue_scroll.scroll_to_item(at.saturating_sub(2), gpui::ScrollStrategy::Top);
        }
        cx.notify();
    }

    pub(crate) fn queue_view(&self, t: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let title = |sub: String| {
            div()
                .px(px(PAD_X))
                .pt(px(28.0))
                .pb(px(14.0))
                .flex()
                .items_baseline()
                .gap(px(14.0))
                .child(div().text_size(px(30.0)).font_weight(FontWeight::BOLD).text_color(rgb(t.text_strong)).child("Queue"))
                .child(div().font_family(MONO).text_size(px(13.0)).text_color(rgb(t.muted)).child(sub))
        };
        let Some(n) = self.now.as_ref() else {
            return div().flex().flex_col().child(title("nothing playing".into())).into_any_element();
        };
        let left: u64 = n.tracks.iter().skip(n.idx + 1).map(|t| t.duration_ms).sum::<u64>() + n.tracks.get(n.idx).map(|t| t.duration_ms.saturating_sub(n.elapsed_ms)).unwrap_or(0);
        let upcoming = n.tracks.len().saturating_sub(n.idx + 1);
        let sub = format!("{} · {upcoming} up next · {} left", n.context, fmt_ms(left));

        let pill = |id: &'static str, label: String, on: bool| {
            div()
                .id(id)
                .h(px(34.0))
                .px(px(14.0))
                .flex()
                .items_center()
                .rounded(px(17.0))
                .border_1()
                .border_color(rgb(if on { t.accent } else { t.line }))
                .text_size(px(13.0))
                .text_color(rgb(if on { t.accent } else { t.text }))
                .cursor_pointer()
                .hover(|d| d.bg(rgb(t.raised)))
                .child(label)
        };
        let after_track = self.stop_after == Some(n.idx) && !self.stop_after_album;
        let after_album = self.stop_after.is_some() && self.stop_after_album;
        let save_title = n.context.clone();
        let actions = div()
            .px(px(PAD_X))
            .pb(px(14.0))
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(10.0))
            .child(pill("q-stop-track", "Stop after this track".into(), after_track).on_click(cx.listener(|this, _, _, cx| this.toggle_stop_after(false, cx))))
            .child(pill("q-stop-album", "Stop after this album".into(), after_album).on_click(cx.listener(|this, _, _, cx| this.toggle_stop_after(true, cx))))
            .child(div().flex_1())
            .when(upcoming > 0, |d| d.child(pill("q-clear", "Clear upcoming".into(), false).on_click(cx.listener(|this, _, _, cx| this.clear_upcoming(cx)))))
            .child(pill("q-save", "Save as playlist".into(), false).on_click(cx.listener(move |this, _, window, cx| {
                let tracks = this.now.as_ref().map(|n| n.tracks.clone()).unwrap_or_default();
                this.save_tracks_as_playlist(&tracks, save_title.clone(), window, cx);
            })));

        let count = entries(n.idx, n.tracks.len()).len();
        let list = uniform_list(
            "queue-list",
            count,
            cx.processor(move |this, range: std::ops::Range<usize>, _window, cx| {
                let t = this.theme.clone();
                let Some(n) = this.now.as_ref() else { return Vec::new() };
                let (idx, queue_id, refill) = (n.idx, n.queue_id, n.refill.is_some());
                let first = this.first_editable();
                let all = entries(idx, n.tracks.len());
                let rows: Vec<(Entry, Option<crate::plex::Track>)> = range
                    .filter_map(|r| all.get(r).copied())
                    .map(|e| match e {
                        Entry::Track(i) => (e, n.tracks.get(i).cloned()),
                        Entry::Head(_) => (e, None),
                    })
                    .collect();
                rows.into_iter()
                    .enumerate()
                    .map(|(k, (e, tr))| match (e, tr) {
                        (Entry::Head(h), _) => div()
                            .id(("q-head", k))
                            .w_full()
                            .h(px(52.0))
                            .px(px(12.0))
                            .flex()
                            .items_end()
                            .pb(px(8.0))
                            .font_family(MONO)
                            .font_weight(FontWeight::MEDIUM)
                            .text_size(px(11.0))
                            .text_color(rgb(t.text2))
                            .child(if h == "NEXT UP" && refill { "NEXT UP · RADIO KEEPS GOING" } else { h })
                            .into_any_element(),
                        (Entry::Track(i), Some(tr)) => {
                            this.request_thumb(&tr.thumb);
                            let art = match this.art.get(&tr.thumb).cloned() {
                                Some(im) => img(im).size(px(36.0)).rounded(px(4.0)).object_fit(ObjectFit::Cover).into_any_element(),
                                None => gpui::img(this.mark(crate::art::placeholder_for(&tr.thumb))).size(px(36.0)).rounded(px(4.0)).object_fit(ObjectFit::Cover).into_any_element(),
                            };
                            let playing = i == idx;
                            let played = i < idx;
                            let editable = i >= first && i > idx;
                            let stop_line = this.stop_after == Some(i) && i + 1 < this.now.as_ref().map(|n| n.tracks.len()).unwrap_or(0);
                            let (bg, border, text) = (t.raised, t.line, t.text);
                            let gtitle = tr.title.clone();
                            let dkey = tr.rating_key.clone();
                            div()
                                .id(("q-row", i))
                                .group("q-row")
                                .w_full()
                                .h(px(52.0))
                                .px(px(12.0))
                                .flex()
                                .items_center()
                                .gap(px(12.0))
                                .rounded(px(8.0))
                                .cursor_pointer()
                                .when(playing, |d| d.bg(rgb(t.raised)))
                                .when(played, |d| d.opacity(0.55))
                                .when(stop_line, |d| d.border_b_2().border_color(rgb(t.red)))
                                .hover(|d| d.bg(rgb(t.raised)))
                                .when(editable, |d| {
                                    let accent = t.accent;
                                    // The whole row drags (a click without movement still plays).
                                    d.on_drag(QueueDrag { from: i, key: dkey.clone(), queue_id }, move |_, _, _, cx| {
                                        let title = gtitle.clone();
                                        cx.new(|_| QueueGhost { title, bg, border, text })
                                    })
                                    .drag_over::<QueueDrag>(move |st, dr, _, _| {
                                        if dr.queue_id != queue_id || dr.from == i {
                                            st
                                        } else if dr.from < i {
                                            st.border_b_2().border_color(rgb(accent))
                                        } else {
                                            st.border_t_2().border_color(rgb(accent))
                                        }
                                    })
                                    .on_drop(cx.listener(move |this, dr: &QueueDrag, _, cx| {
                                        let first = this.first_editable();
                                        // Where the dragged track is now, nearest its old place.
                                        let from = this.now.as_ref().filter(|n| n.queue_id == dr.queue_id).and_then(|n| {
                                            let at = |j: usize| n.tracks.get(j).is_some_and(|t| t.rating_key == dr.key);
                                            if at(dr.from) { Some(dr.from) } else { (first..n.tracks.len()).filter(|&j| at(j)).min_by_key(|&j| j.abs_diff(dr.from)) }
                                        });
                                        if let Some(from) = from {
                                            this.move_upcoming(from, i, cx);
                                        }
                                    }))
                                })
                                .child(
                                    div()
                                        .w(px(36.0))
                                        .h_full()
                                        .relative()
                                        .flex()
                                        .items_center()
                                        .font_family(MONO)
                                        .text_size(px(12.0))
                                        .text_color(rgb(if playing { t.accent } else { t.muted }))
                                        .child(div().when(!playing, |d| d.group_hover("q-row", |s| s.opacity(0.0))).child(if playing { "▶".to_string() } else { (i + 1).to_string() }))
                                        // On hover the number gives way to a play button
                                        // (the playing row keeps its marker).
                                        .when(!playing, |d| d.child(
                                            div()
                                                .id(("q-play", i))
                                                .absolute()
                                                .left(px(-6.0))
                                                .top_0()
                                                .bottom_0()
                                                .w(px(30.0))
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .opacity(0.0)
                                                .group_hover("q-row", |s| s.opacity(1.0))
                                                .child(icons::icon("play", 14.0, t.text))
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    cx.stop_propagation();
                                                    this.play_from(i, 0, cx);
                                                })),
                                        )),
                                )
                                .child(div().flex_none().child(art))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .flex()
                                        .flex_col()
                                        .gap(px(3.0))
                                        .child(div().text_size(px(14.0)).font_weight(FontWeight::MEDIUM).text_color(rgb(if playing { t.accent } else { t.text })).truncate().child(tr.title.clone()))
                                        .child(div().text_size(px(12.0)).text_color(rgb(t.muted)).truncate().child(format!("{} · {}", tr.artist, tr.album_title))),
                                )
                                .when(stop_line, |d| d.child(div().font_family(MONO).text_size(px(10.0)).text_color(rgb(t.red)).child("STOPS HERE")))
                                .child(div().w(px(56.0)).flex().justify_end().font_family(MONO).text_size(px(12.0)).text_color(rgb(t.muted)).child(fmt_ms(tr.duration_ms)))
                                .child(
                                    div().w(px(28.0)).flex().justify_center().when(editable, |d| {
                                        d.child(
                                            div()
                                                .id(("q-rm", i))
                                                .size(px(24.0))
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .rounded(px(6.0))
                                                .opacity(0.0)
                                                .group_hover("q-row", |s| s.opacity(1.0))
                                                .hover(|d| d.bg(rgb(t.surface)))
                                                .text_size(px(15.0))
                                                .text_color(rgb(t.muted))
                                                .child("×")
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    cx.stop_propagation();
                                                    this.remove_upcoming(i, cx);
                                                })),
                                        )
                                    }),
                                )
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if this.now.as_ref().is_some_and(|n| n.idx != i) {
                                        this.play_from(i, 0, cx);
                                    }
                                }))
                                .into_any_element()
                        }
                        _ => div().h(px(52.0)).into_any_element(),
                    })
                    .collect()
            }),
        )
        .track_scroll(&self.queue_scroll)
        .flex_1()
        .px(px(PAD_X));

        div().size_full().flex().flex_col().child(title(sub)).child(actions).child(list).into_any_element()
    }
}
