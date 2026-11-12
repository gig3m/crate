//! A playlist's page: header, Play / Shuffle, and its tracks (virtualized:
//! smart playlists run to a thousand tracks). Regular playlists can drop and
//! reorder tracks (drag a row, or the row's right-click menu); smart ones
//! are what their rules say.

use std::sync::Arc;

use gpui::{Context, FontWeight, ObjectFit, div, img, linear_color_stop, linear_gradient, prelude::*, px, rgb, uniform_list};
use gpui_component::menu::{ContextMenuExt as _, PopupMenuItem};

use crate::{Crate, PAD_X, View, fmt_ms, icons, plex::PlaylistDetail, shell::MONO, theme::{Theme, mix}};

/// Most tracks a playlist page loads.
const LIMIT: usize = 5000;

/// One edit to a regular playlist, by playlist item id.
#[derive(Clone, Debug)]
pub(crate) enum PlOp {
    Remove { item: u64 },
    /// Put `item` just after `after`, or at the top.
    Move { item: u64, after: Option<u64> },
}

#[derive(Clone, Debug)]
pub(crate) struct PlJob {
    pub key: String,
    pub op: PlOp,
    /// The track's title, for messages.
    pub title: String,
}

/// What a dragged playlist row carries.
#[derive(Clone)]
struct RowDrag {
    key: String,
    item: u64,
    /// Row index when the drag started (for the drop line only).
    from: usize,
    /// Where the pointer was when the drag began (GPUI starts it 2px from
    /// the press), to tell a wobbled click from a drag that came back.
    start: std::rc::Rc<std::cell::Cell<Option<gpui::Point<gpui::Pixels>>>>,
}

/// The card that follows the pointer while a row is dragged.
struct DragGhost {
    title: String,
    artist: String,
    bg: u32,
    border: u32,
    text: u32,
    muted: u32,
    /// How far into the row it was grabbed, so the card stays under the pointer.
    grab_x: gpui::Pixels,
}

impl gpui::Render for DragGhost {
    fn render(&mut self, _: &mut gpui::Window, _: &mut Context<Self>) -> impl IntoElement {
        div().pl((self.grab_x - px(24.0)).max(px(0.0))).child(
            div()
                .w(px(280.0))
                .px(px(12.0))
                .py(px(8.0))
                .rounded(px(8.0))
                .bg(rgb(self.bg))
                .border_1()
                .border_color(rgb(self.border))
                .shadow_lg()
                .opacity(0.95)
                .child(div().text_size(px(14.0)).font_weight(FontWeight::MEDIUM).text_color(rgb(self.text)).truncate().child(self.title.clone()))
                .child(div().text_size(px(12.0)).text_color(rgb(self.muted)).truncate().child(self.artist.clone())),
        )
    }
}

impl Crate {
    pub(crate) fn open_playlist(&mut self, key: String, title: String, cx: &mut Context<Self>) {
        self.playlist_scroll = gpui::UniformListScrollHandle::new();
        self.go(View::Playlist { key: key.clone(), title, detail: None });
        self.load_playlist(key, cx);
        cx.notify();
    }

    pub(crate) fn load_playlist(&mut self, key: String, cx: &mut Context<Self>) {
        self.playlist_load_gen += 1;
        let gen_ = self.playlist_load_gen;
        if self.playlist_error.as_ref().is_some_and(|(k, _)| *k == key) {
            self.playlist_error = None;
        }
        let k = key.clone();
        let rx = crate::plex_call(move |c| async move {
            tokio::time::timeout(std::time::Duration::from_secs(30), c.playlist_detail(&k, LIMIT))
                .await
                .map_err(|_| anyhow::anyhow!("Plex didn't answer"))?
        });
        cx.spawn(async move |this, cx| {
            let res = rx.await;
            this.update(cx, |this, cx| {
                // A newer load (another removal, a revisit) supersedes this one.
                if this.playlist_load_gen != gen_ {
                    return;
                }
                match res {
                    Ok(Ok(d)) => {
                        // While edits are still going to Plex the server's order
                        // is half-applied: keep what's on screen (or keep
                        // loading); the last edit reloads.
                        if this.playlist_pending(&key) {
                            return;
                        }
                        this.request_thumb(&d.thumb);
                        if let View::Playlist { key: cur, detail, .. } = &mut this.view
                            && *cur == key
                        {
                            *detail = Some(Arc::new(d));
                        }
                    }
                    Ok(Err(e)) => this.playlist_error = Some((key, format!("{e:#}"))),
                    Err(_) => this.playlist_error = Some((key, "the request was dropped".into())),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The queue name for the playlist on screen: the server's title.
    fn playlist_queue_name(view_title: &str, d: &PlaylistDetail) -> String {
        if d.title.is_empty() { view_title.to_string() } else { d.title.clone() }
    }

    /// The loaded tracks of the playlist on screen, and its queue name.
    fn playlist_tracks_shown(&self) -> Option<(String, Arc<PlaylistDetail>)> {
        match &self.view {
            View::Playlist { title, detail: Some(d), .. } => Some((Self::playlist_queue_name(title, d), d.clone())),
            _ => None,
        }
    }

    /// Test hook: remove row `i` of the playlist on screen.
    pub(crate) fn remove_playlist_row(&mut self, i: usize, cx: &mut Context<Self>) {
        if let View::Playlist { key, detail: Some(d), .. } = &self.view
            && let (Some(tr), Some(&id)) = (d.tracks.get(i), d.item_ids.get(i))
        {
            let (key, title) = (key.clone(), tr.title.clone());
            self.remove_playlist_item(key, id, title, cx);
        }
    }

    pub(crate) fn play_playlist_from(&mut self, i: usize, shuffle: bool, cx: &mut Context<Self>) {
        /// Longest queue a playlist starts (from the clicked row on).
        const QUEUE_MAX: usize = 1000;
        let Some((title, d)) = self.playlist_tracks_shown() else { return };
        let View::Playlist { key, .. } = &self.view else { return };
        let ck = Some(format!("playlist:{key}"));
        if d.tracks.is_empty() {
            return;
        }
        if shuffle {
            let mut tracks = d.tracks.clone();
            crate::shuffle(&mut tracks);
            tracks.truncate(QUEUE_MAX);
            self.start_queue(format!("{title} (shuffled)"), ck, tracks, 0, cx);
        } else if d.tracks.len() <= QUEUE_MAX {
            self.start_queue(title, ck, d.tracks.clone(), i, cx);
        } else {
            let tracks: Vec<_> = d.tracks.iter().skip(i).take(QUEUE_MAX).cloned().collect();
            self.start_queue(title, ck, tracks, 0, cx);
        }
    }

    fn remove_playlist_item(&mut self, key: String, item_id: u64, title: String, cx: &mut Context<Self>) {
        // Off the page at once (no double removes); Plex gets it in turn.
        if let View::Playlist { key: cur, detail: Some(d), .. } = &mut self.view
            && *cur == key
            && let Some(pos) = d.item_ids.iter().position(|&x| x == item_id)
        {
            let d = Arc::make_mut(d);
            d.item_ids.remove(pos);
            d.tracks.remove(pos);
            d.count = d.count.saturating_sub(1);
            d.loaded = d.loaded.saturating_sub(1);
        } else {
            return;
        }
        self.queue_playlist_op(PlJob { key, op: PlOp::Remove { item: item_id }, title }, cx);
    }

    /// Test hook: move row `from` of the playlist on screen to position `to`.
    pub(crate) fn move_playlist_row_pub(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        if let View::Playlist { key, detail: Some(d), .. } = &self.view
            && let Some(&item) = d.item_ids.get(from)
        {
            let key = key.clone();
            self.move_playlist_item(key, item, to, cx);
        }
    }

    /// Move a regular playlist's item to row `to` of the order on screen.
    fn move_playlist_item(&mut self, key: String, item: u64, to: usize, cx: &mut Context<Self>) {
        let View::Playlist { key: cur, detail: Some(d), .. } = &mut self.view else { return };
        if *cur != key || d.smart {
            return;
        }
        let Some(from) = d.item_ids.iter().position(|&x| x == item) else { return };
        let to = to.min(d.item_ids.len() - 1);
        if from == to {
            return;
        }
        let d = Arc::make_mut(d);
        let id = d.item_ids.remove(from);
        let tr = d.tracks.remove(from);
        d.item_ids.insert(to, id);
        let title = tr.title.clone();
        d.tracks.insert(to, tr);
        let after = to.checked_sub(1).map(|p| d.item_ids[p]);
        self.queue_playlist_op(PlJob { key, op: PlOp::Move { item, after }, title }, cx);
        cx.notify();
    }

    /// Edits for `key` not yet confirmed by Plex.
    pub(crate) fn playlist_pending(&self, key: &str) -> bool {
        self.playlist_op_running.as_deref() == Some(key) || self.playlist_ops.iter().any(|j| j.key == key)
    }

    fn queue_playlist_op(&mut self, job: PlJob, cx: &mut Context<Self>) {
        self.forget_playlist_history(&job.key);
        self.playlist_ops.push_back(job);
        if self.playlist_op_running.is_none() {
            self.run_playlist_op(cx);
        }
    }

    /// Send the next queued edit; when a playlist's last one lands, show
    /// what the server has.
    fn run_playlist_op(&mut self, cx: &mut Context<Self>) {
        let Some(job) = self.playlist_ops.pop_front() else {
            self.playlist_op_running = None;
            return;
        };
        self.playlist_op_running = Some(job.key.clone());
        let (k, op) = (job.key.clone(), job.op.clone());
        let rx = crate::plex_call(move |c| async move {
            match op {
                PlOp::Remove { item } => c.remove_from_playlist(&k, item).await,
                PlOp::Move { item, after } => c.move_playlist_item(&k, item, after).await,
            }
        });
        cx.spawn(async move |this, cx| {
            let res = rx.await;
            this.update(cx, |this, cx| {
                let PlJob { key, op, title } = job;
                let failed = !matches!(res, Ok(Ok(())));
                match (&op, res) {
                    (PlOp::Remove { .. }, Ok(Ok(()))) => this.say(format!("Removed \u{201c}{title}\u{201d}")),
                    (PlOp::Remove { .. }, Ok(Err(e))) => this.say(format!("Couldn't remove \u{201c}{title}\u{201d}: {e:#}")),
                    (PlOp::Move { .. }, Ok(Err(e))) => this.say(format!("Couldn't move \u{201c}{title}\u{201d}: {e:#}")),
                    (_, Err(_)) => this.say(format!("Couldn't save the change to \u{201c}{title}\u{201d}: the request was dropped")),
                    _ => {}
                }
                // Later edits were made against the order this one left; after
                // a failure they'd land in the wrong places.
                if failed {
                    let dropped = this.playlist_ops.iter().filter(|j| j.key == key).count();
                    this.playlist_ops.retain(|j| j.key != key);
                    if dropped > 0 {
                        this.say(format!("Couldn't save the playlist changes; showing what Plex has ({dropped} more not sent)"));
                    }
                }
                this.playlist_op_running = None;
                if !this.playlist_pending(&key) {
                    // Copies pushed to history meanwhile are stale now. (Not
                    // before: a refetch mid-sequence would show a half-applied
                    // order to edit against.)
                    this.forget_playlist_history(&key);
                    if matches!(&this.view, View::Playlist { key: cur, .. } if *cur == key) {
                        this.load_playlist(key.clone(), cx);
                    }
                    if matches!(op, PlOp::Remove { .. }) || failed {
                        this.reload_playlists(cx);
                    }
                }
                this.run_playlist_op(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Scroll the track list while a dragged row sits near its top or
    /// bottom edge, faster the closer it is.
    fn playlist_edge_scroll(&mut self, speed: f32, cx: &mut Context<Self>) {
        self.playlist_edge = speed;
        if speed == 0.0 || self.playlist_edge_ticking {
            return;
        }
        self.playlist_edge_ticking = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(std::time::Duration::from_millis(16)).await;
                let go = this
                    .update(cx, |this, cx| {
                        if this.playlist_edge == 0.0 || !cx.has_active_drag() || !matches!(this.view, View::Playlist { .. }) {
                            this.playlist_edge = 0.0;
                            this.playlist_edge_ticking = false;
                            return false;
                        }
                        let h = this.playlist_scroll.0.borrow().base_handle.clone();
                        let (off, max) = (h.offset(), h.max_offset());
                        let y = (off.y - px(this.playlist_edge)).clamp(-max.y, px(0.0));
                        if y != off.y {
                            h.set_offset(gpui::point(off.x, y));
                            cx.notify();
                        }
                        true
                    })
                    .unwrap_or(false);
                if !go {
                    break;
                }
            }
        })
        .detach();
    }

    pub(crate) fn rename_playlist(&mut self, key: String, title: String, cx: &mut Context<Self>) {
        self.playlist_busy = true;
        let (k, t) = (key.clone(), title.clone());
        let rx = crate::plex_call(move |c| async move { c.rename_playlist(&k, &t).await });
        cx.spawn(async move |this, cx| {
            let res = rx.await;
            this.update(cx, |this, cx| {
                this.playlist_busy = false;
                match res {
                    Ok(Ok(())) => {
                        this.say(format!("Renamed to \u{201c}{title}\u{201d}"));
                        for v in std::iter::once(&mut this.view).chain(this.back.iter_mut()).chain(this.fwd.iter_mut()) {
                            if let View::Playlist { key: cur, title: t, detail } = v
                                && *cur == key
                            {
                                *t = title.clone();
                                if let Some(d) = detail {
                                    Arc::make_mut(d).title = title.clone();
                                }
                            }
                        }
                        this.forget_playlist_history(&key);
                        if matches!(&this.view, View::Playlist { key: cur, .. } if *cur == key) {
                            this.load_playlist(key, cx);
                        }
                        this.reload_playlists(cx);
                    }
                    Ok(Err(e)) => this.say(format!("Couldn't rename: {e:#}")),
                    Err(_) => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Test hook entry for the Delete button.
    pub(crate) fn delete_playlist_clicked_pub(&mut self, key: String, title: String, cx: &mut Context<Self>) {
        self.delete_playlist_clicked(key, title, cx);
    }

    /// Delete takes two clicks: the first arms it for a few seconds.
    fn delete_playlist_clicked(&mut self, key: String, title: String, cx: &mut Context<Self>) {
        let armed = self.playlist_delete_armed.as_ref().is_some_and(|(k, at)| *k == key && at.elapsed() < std::time::Duration::from_secs(4));
        if self.playlist_busy || self.playlist_pending(&key) {
            self.say("Still saving the last playlist change…");
            return;
        }
        if !armed {
            self.playlist_delete_armed = Some((key, std::time::Instant::now()));
            self.say(format!("Click Delete again to delete \u{201c}{title}\u{201d} (its tracks stay in your library)"));
            // Repaint when the arm runs out, so the button reads Delete again.
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(std::time::Duration::from_millis(4050)).await;
                this.update(cx, |_, cx| cx.notify()).ok();
            })
            .detach();
            cx.notify();
            return;
        }
        self.playlist_delete_armed = None;
        self.playlist_busy = true;
        let k = key.clone();
        let rx = crate::plex_call(move |c| async move { c.delete_playlist(&k).await });
        cx.spawn(async move |this, cx| {
            let res = rx.await;
            this.update(cx, |this, cx| {
                this.playlist_busy = false;
                match res {
                    Ok(Ok(())) => {
                        this.say(format!("Deleted \u{201c}{title}\u{201d}"));
                        // Drop it from history, and leave its page (closing a
                        // naming bar opened there) for the Playlists page.
                        let gone = |v: &View| matches!(v, View::Playlist { key: k, .. } if *k == key);
                        this.back.retain(|v| !gone(v));
                        this.fwd.retain(|v| !gone(v));
                        if gone(&this.view) {
                            if matches!(this.back.last(), Some(View::Soon("playlists"))) {
                                this.go_back_pub(cx);
                            } else {
                                this.go_pub(View::Soon("playlists"));
                            }
                            // Navigating pushed the deleted page into history.
                            this.back.retain(|v| !gone(v));
                            this.fwd.retain(|v| !gone(v));
                        }
                        this.reload_playlists(cx);
                        this.unpin_quietly(crate::pins::PinKind::Playlist, &key);
                    }
                    Ok(Err(e)) => this.say(format!("Couldn't delete \u{201c}{title}\u{201d}: {e:#}")),
                    Err(_) => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// After a change made elsewhere (tracks added): refetch the page if it's
    /// on screen, and have history copies refetch on return. While edits are
    /// pending the last one reloads instead.
    pub(crate) fn refresh_playlist_page(&mut self, key: &str, cx: &mut Context<Self>) {
        if self.playlist_pending(key) {
            return;
        }
        self.forget_playlist_history(key);
        if matches!(&self.view, View::Playlist { key: cur, .. } if cur == key) {
            self.load_playlist(key.to_string(), cx);
        }
    }

    /// Copies of a playlist page in Back/Forward history refetch on return.
    fn forget_playlist_history(&mut self, key: &str) {
        for v in self.back.iter_mut().chain(self.fwd.iter_mut()) {
            if let View::Playlist { key: k, detail, .. } = v
                && k == key
            {
                *detail = None;
            }
        }
    }

    pub(crate) fn playlist_view(&self, key: &str, title: &str, detail: Option<&PlaylistDetail>, t: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        // While loading, the sidebar's entry gives the cover and count.
        let title = detail.map(|d| d.title.as_str()).filter(|s| !s.is_empty()).unwrap_or(title);
        let listed = self.home.as_ref().and_then(|h| h.playlists.iter().find(|p| p.rating_key == key)).cloned();
        let thumb = detail.map(|d| d.thumb.clone()).or(listed.as_ref().map(|p| p.thumb.clone())).unwrap_or_default();
        let smart = detail.map(|d| d.smart).or(listed.as_ref().map(|p| p.smart)).unwrap_or(false);
        let field = mix(t.surface, t.discovery, 0.12);
        let cover = match self.art.get(&thumb).cloned() {
            Some(i) => img(i).size(px(200.0)).rounded(px(8.0)).object_fit(ObjectFit::Cover).into_any_element(),
            None => gpui::img(self.mark(crate::art::placeholder_for(&thumb))).size(px(200.0)).rounded(px(8.0)).object_fit(ObjectFit::Cover).into_any_element(),
        };
        let facts = match detail {
            Some(d) => {
                let n = d.count.max(d.tracks.len() as u64);
                let mins = if d.duration_ms > 0 { d.duration_ms } else { d.tracks.iter().map(|t| t.duration_ms).sum() } / 60_000;
                let missing = d.loaded.saturating_sub(d.tracks.len());
                let mut more = String::new();
                if missing > 0 {
                    more += &format!(" · {missing} unavailable (no file on the server)");
                }
                if d.capped {
                    more += &format!(" · showing the first {LIMIT}");
                }
                let len = if mins >= 60 { format!("{} h {} min", mins / 60, mins % 60) } else { format!("{mins} min") };
                format!("{n} tracks, {len}{more}")
            }
            None if self.playlist_error.as_ref().is_some_and(|(k, _)| k == key) => {
                format!("Couldn't load: {}", self.playlist_error.as_ref().map(|(_, e)| e.as_str()).unwrap_or_default())
            }
            None => listed.map(|p| format!("{} tracks · loading…", p.count)).unwrap_or_else(|| "Loading…".into()),
        };
        let failed = detail.is_none() && self.playlist_error.as_ref().is_some_and(|(k, _)| k == key);
        let retry_key = key.to_string();
        let header = div()
            .pt(px(24.0))
            .pb(px(20.0))
            .px(px(PAD_X))
            .flex()
            .gap(px(28.0))
            .items_end()
            .bg(linear_gradient(180.0, linear_color_stop(rgb(field), 0.0), linear_color_stop(rgb(t.base), 1.0)))
            .child(div().flex_none().shadow_2xl().child(cover))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .child(
                        div()
                            .font_family(MONO)
                            .font_weight(FontWeight::MEDIUM)
                            .text_size(px(11.0))
                            .text_color(rgb(t.text2))
                            .child(if smart { "SMART PLAYLIST · FOLLOWS ITS RULES" } else { "PLAYLIST" }),
                    )
                    .child(
                        div()
                            .text_size(px(if title.len() > 28 { 36.0 } else { 52.0 }))
                            .line_height(gpui::relative(1.05))
                            .font_weight(FontWeight::BOLD)
                            .text_color(rgb(t.text_strong))
                            .line_clamp(2)
                            .child(title.to_string()),
                    )
                    .child(div().text_size(px(14.0)).text_color(rgb(t.text2)).child(facts))
                    .when(failed, |d| {
                        d.child(
                            div()
                                .id("pl-retry")
                                .w(px(80.0))
                                .h(px(30.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(8.0))
                                .border_1()
                                .border_color(rgb(t.line))
                                .text_size(px(13.0))
                                .text_color(rgb(t.text))
                                .cursor_pointer()
                                .hover(|d| d.bg(rgb(t.raised)))
                                .child("Retry")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.load_playlist(retry_key.clone(), cx);
                                    cx.notify();
                                })),
                        )
                    }),
            );

        let this_pin = self.pin_pill(
            "pl-pin",
            crate::pins::Pin { kind: crate::pins::PinKind::Playlist, key: key.to_string(), title: title.to_string(), thumb: thumb.clone(), mix: None, desc: String::new() },
            t,
            cx,
        );
        let actions = div()
            .px(px(PAD_X))
            .pb(px(16.0))
            .flex()
            .items_center()
            .gap(px(12.0))
            .child(
                div()
                    .id("pl-play")
                    .size(px(52.0))
                    .rounded(px(26.0))
                    .bg(rgb(t.accent))
                    .flex()
                    .items_center()
                    .justify_center()
                    .shadow_lg()
                    .cursor_pointer()
                    .hover(|d| d.opacity(0.9))
                    .child(icons::icon("play", 22.0, t.base))
                    .on_click(cx.listener(|this, _, _, cx| this.play_playlist_from(0, false, cx))),
            )
            .child(
                div()
                    .id("pl-shuffle")
                    .size(px(44.0))
                    .rounded(px(22.0))
                    .border_1()
                    .border_color(rgb(t.line))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(|d| d.bg(rgb(t.raised)))
                    .child(icons::icon("shuffle", 18.0, t.text))
                    .on_click(cx.listener(|this, _, _, cx| this.play_playlist_from(0, true, cx))),
            )
            .child(div().flex_1())
            .when(detail.is_some(), |d| {
                let (k1, t1, k2, t2) = (key.to_string(), title.to_string(), key.to_string(), title.to_string());
                let armed = self.playlist_delete_armed.as_ref().is_some_and(|(k, at)| k == key && at.elapsed() < std::time::Duration::from_secs(4));
                let pill = |id: &'static str, label: &'static str, color: u32| {
                    div()
                        .id(id)
                        .h(px(34.0))
                        .px(px(14.0))
                        .flex()
                        .items_center()
                        .rounded(px(17.0))
                        .border_1()
                        .border_color(rgb(t.line))
                        .text_size(px(13.0))
                        .text_color(rgb(color))
                        .cursor_pointer()
                        .hover(|d| d.bg(rgb(t.raised)))
                        .child(label)
                };
                d.child(this_pin).child(pill("pl-rename", "Rename", t.text).on_click(cx.listener(move |this, _, window, cx| {
                    this.start_naming(crate::playlist_add::NameFor::Rename(k1.clone()), t1.clone(), window, cx);
                })))
                .child(pill("pl-delete", if armed { "Click again to delete" } else { "Delete" }, if armed { t.red } else { t.text2 }).on_click(cx.listener(
                    move |this, _, _, cx| this.delete_playlist_clicked(k2.clone(), t2.clone(), cx),
                )))
            });

        let head = div()
            .mx(px(PAD_X))
            .px(px(12.0))
            .pb(px(8.0))
            .flex()
            .gap(px(12.0))
            .font_family(MONO)
            .font_weight(FontWeight::MEDIUM)
            .text_size(px(11.0))
            .text_color(rgb(t.muted))
            .border_b_1()
            .border_color(rgb(t.line))
            .child(div().w(px(40.0)).child("#"))
            .child(div().flex_1().child("TITLE"))
            .child(div().w(px(220.0)).child("ALBUM"))
            .child(div().w(px(110.0)).child("RATING"))
            .child(div().w(px(64.0)).flex().justify_end().child("TIME"))
            .child(div().w(px(24.0)))
            .child(div().w(px(28.0)));

        let n = detail.map(|d| d.tracks.len()).unwrap_or(0);
        let list = uniform_list(
            "playlist-tracks",
            n,
            cx.processor(move |this, range: std::ops::Range<usize>, _window, cx| {
                let t = this.theme.clone();
                let View::Playlist { key, detail: Some(d), .. } = &this.view else { return Vec::new() };
                let (key, smart) = (key.clone(), d.smart);
                // The row playing now, when this playlist is what's playing:
                // by playlist item, so a track listed twice lights up once.
                let ck = format!("playlist:{key}");
                let playing_item = this
                    .now
                    .as_ref()
                    .filter(|n| n.context_key.as_deref() == Some(ck.as_str()))
                    .and_then(|n| n.tracks.get(n.idx))
                    .map(|tr| tr.playlist_item)
                    .filter(|&id| id > 0);
                let reorder = !smart && d.item_ids.len() > 1;
                let last = d.item_ids.len().saturating_sub(1);
                // Capped at LIMIT: the last row shown isn't the playlist's last.
                let capped = d.capped;
                let rows: Vec<(usize, crate::plex::Track, u64)> = range.filter_map(|i| Some((i, d.tracks.get(i)?.clone(), d.item_ids.get(i).copied().unwrap_or(0)))).collect();
                rows.into_iter()
                    .map(|(i, tr, item_id)| {
                        this.request_thumb(&tr.thumb);
                        let art = match this.art.get(&tr.thumb).cloned() {
                            Some(im) => img(im).size(px(36.0)).rounded(px(4.0)).object_fit(ObjectFit::Cover).into_any_element(),
                            None => gpui::img(this.mark(crate::art::placeholder_for(&tr.thumb))).size(px(36.0)).rounded(px(4.0)).object_fit(ObjectFit::Cover).into_any_element(),
                        };
                        let playing = playing_item.is_some() && playing_item == Some(item_id);
                        let (k, name) = (key.clone(), tr.title.clone());
                        let num = div()
                            .w(px(40.0))
                            .h_full()
                            .relative()
                            .flex()
                            .items_center()
                            .font_family(MONO)
                            .font_weight(FontWeight::MEDIUM)
                            .text_size(px(13.0))
                            .text_color(rgb(if playing { t.accent } else { t.muted }))
                            .child(div().group_hover("pl-row", |s| s.opacity(0.0)).child(if playing { "▶".to_string() } else { (i + 1).to_string() }))
                            // On hover the number gives way to a play button.
                            .child(
                                div()
                                    .id(("pl-play", i))
                                    .absolute()
                                    .left(px(-6.0))
                                    .top_0()
                                    .bottom_0()
                                    .w(px(30.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .opacity(0.0)
                                    .group_hover("pl-row", |s| s.opacity(1.0))
                                    .child(icons::icon("play", 14.0, t.text))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.play_playlist_from(i, false, cx);
                                    })),
                            );
                        let row = div()
                            .id(("pl-track", i))
                            .group("pl-row")
                            .h(px(52.0))
                            .w_full()
                            .px(px(12.0))
                            .flex()
                            .items_center()
                            .gap(px(12.0))
                            .rounded(px(8.0))
                            .cursor_pointer()
                            .when(playing, |d| d.bg(rgb(t.raised)))
                            .hover(|d| d.bg(rgb(t.raised)))
                            .when(reorder && item_id > 0, |d| {
                                let (accent, kk) = (t.accent, key.clone());
                                let (k2, target) = (key.clone(), item_id);
                                // The whole row drags (a click without movement still plays).
                                let drag = RowDrag { key: key.clone(), item: item_id, from: i, start: Default::default() };
                                let (gt, ga) = (tr.title.clone(), tr.artist.clone());
                                let (bg, border, text, muted) = (t.raised, t.line, t.text, t.muted);
                                d.on_drag(drag, move |dr, at, window, cx| {
                                    dr.start.set(Some(window.mouse_position()));
                                    let (title, artist) = (gt.clone(), ga.clone());
                                    cx.new(|_| DragGhost { title, artist, bg, border, text, muted, grab_x: at.x })
                                })
                                .drag_over::<RowDrag>(move |st, dr, _, _| {
                                    // A line where the row will land: below
                                    // this one moving down, above moving up.
                                    if dr.key != kk || dr.item == target {
                                        st
                                    } else if dr.from < i {
                                        st.border_b_2().border_color(rgb(accent))
                                    } else {
                                        st.border_t_2().border_color(rgb(accent))
                                    }
                                })
                                .on_drop(cx.listener(move |this, dr: &RowDrag, window, cx| {
                                    if dr.key != k2 {
                                        return;
                                    }
                                    // Dropped on its own row. Barely moved: a click that wobbled
                                    // past the 2px drag threshold, so play it; otherwise a
                                    // drag brought back to cancel.
                                    if dr.item == target {
                                        if crate::queue_view::wobbled_click(&dr.start, window) {
                                            this.play_playlist_from(i, false, cx);
                                        }
                                        return;
                                    }
                                    let to = match &this.view {
                                        View::Playlist { detail: Some(d), .. } => d.item_ids.iter().position(|&x| x == target),
                                        _ => None,
                                    };
                                    if let Some(to) = to {
                                        this.move_playlist_item(k2.clone(), dr.item, to, cx);
                                    }
                                }))
                            })
                            .child(num)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .items_center()
                                    .gap(px(12.0))
                                    .child(div().flex_none().child(art))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .flex()
                                            .flex_col()
                                            .gap(px(3.0))
                                            .child(
                                                div()
                                                    .text_size(px(14.0))
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .text_color(rgb(if playing { t.accent } else { t.text }))
                                                    .truncate()
                                                    .child(tr.title.clone()),
                                            )
                                            .child(div().text_size(px(12.0)).text_color(rgb(t.muted)).truncate().child(tr.artist.clone())),
                                    ),
                            )
                            .child(div().w(px(220.0)).text_size(px(13.0)).text_color(rgb(t.text2)).truncate().child(tr.album_title.clone()))
                            .child(this.track_stars(format!("pl-r-{i}"), &tr, &t, cx))
                            .child(
                                div()
                                    .w(px(64.0))
                                    .flex()
                                    .justify_end()
                                    .font_family(MONO)
                                    .text_size(px(12.0))
                                    .text_color(rgb(t.text2))
                                    .child(fmt_ms(tr.duration_ms)),
                            )
                            .child(this.playlist_menu_icon(("pl-add", i), vec![tr.rating_key.clone()], tr.title.clone(), t.muted, cx))
                            .child(div().w(px(28.0)).flex().justify_end().when(!smart && item_id > 0, |d| {
                                d.child(
                                    div()
                                        .id(("pl-remove", i))
                                        .size(px(24.0))
                                        .rounded(px(12.0))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .hover(|d| d.bg(rgb(t.line)))
                                        .child(icons::icon("close", 12.0, t.muted))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.remove_playlist_item(k.clone(), item_id, name.clone(), cx);
                                        })),
                                )
                            }))
                            .on_click(cx.listener(move |this, _, _, cx| this.play_playlist_from(i, false, cx)));
                        let entity = cx.entity().downgrade();
                        let (mk, mtr) = (key.clone(), tr.clone());
                        let can_edit = !smart && item_id > 0;
                        let row = row.context_menu(move |mut menu, _, _| {
                            let e = entity.clone();
                            menu = menu.item(PopupMenuItem::new("Play from here").on_click(move |_, _, cx| {
                                e.update(cx, |this, cx| this.play_playlist_from(i, false, cx)).ok();
                            }));
                            for (label, next) in [("Play next", true), ("Add to queue", false)] {
                                let (e, tr) = (entity.clone(), mtr.clone());
                                menu = menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                                    let title = tr.title.clone();
                                    e.update(cx, |this, cx| this.enqueue(vec![tr.clone()], next, title, cx)).ok();
                                }));
                            }
                            if can_edit {
                                menu = menu.separator();
                                if reorder {
                                    for (label, to, show) in [("Move to top", 0, i > 0), ("Move to bottom", last, i < last && !capped)] {
                                        if !show {
                                            continue;
                                        }
                                        let (e, k) = (entity.clone(), mk.clone());
                                        menu = menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                                            e.update(cx, |this, cx| this.move_playlist_item(k.clone(), item_id, to, cx)).ok();
                                        }));
                                    }
                                }
                                let (e, k, n) = (entity.clone(), mk.clone(), mtr.title.clone());
                                menu = menu.item(PopupMenuItem::new("Remove from playlist").on_click(move |_, _, cx| {
                                    e.update(cx, |this, cx| this.remove_playlist_item(k.clone(), item_id, n.clone(), cx)).ok();
                                }));
                            }
                            menu
                        });
                        div().w_full().px(px(PAD_X)).child(row)
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.playlist_scroll)
        .flex_1()
        .pb(px(24.0));

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(header)
            .child(actions)
            .child(head)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .pt(px(4.0))
                    .on_drag_move::<RowDrag>(cx.listener(|this, ev: &gpui::DragMoveEvent<RowDrag>, _, cx| {
                        const ZONE: f32 = 56.0;
                        let (b, y) = (ev.bounds, ev.event.position.y);
                        let (top, bottom) = (f32::from(y - b.top()), f32::from(b.bottom() - y));
                        let speed = if top < ZONE {
                            -(3.0 + 17.0 * ((ZONE - top) / ZONE).min(1.5))
                        } else if bottom < ZONE {
                            3.0 + 17.0 * ((ZONE - bottom) / ZONE).min(1.5)
                        } else {
                            0.0
                        };
                        this.playlist_edge_scroll(speed, cx);
                    }))
                    .when(detail.is_some_and(|d| d.tracks.is_empty()), |d| {
                        d.child(div().px(px(PAD_X + 12.0)).pt(px(12.0)).text_size(px(13.0)).text_color(rgb(t.muted)).child("This playlist is empty."))
                    })
                    .child(list),
            )
            .into_any_element()
    }
}
