//! A playlist's page: header, Play / Shuffle, and its tracks (virtualized:
//! smart playlists run to a thousand tracks). Regular playlists can drop a
//! track; smart ones are what their rules say.

use gpui::{Context, FontWeight, ObjectFit, div, img, linear_color_stop, linear_gradient, prelude::*, px, rgb, uniform_list};

use crate::{Crate, PAD_X, View, fmt_ms, icons, plex::PlaylistDetail, shell::MONO, theme::{Theme, mix}};

/// Most tracks a playlist page loads.
const LIMIT: usize = 5000;

impl Crate {
    pub(crate) fn open_playlist(&mut self, key: String, title: String, cx: &mut Context<Self>) {
        self.playlist_scroll = gpui::UniformListScrollHandle::new();
        self.go(View::Playlist { key: key.clone(), title, detail: None });
        self.load_playlist(key, cx);
        cx.notify();
    }

    pub(crate) fn load_playlist(&mut self, key: String, cx: &mut Context<Self>) {
        let k = key.clone();
        let rx = crate::plex_call(move |c| async move { c.playlist_detail(&k, LIMIT).await });
        cx.spawn(async move |this, cx| {
            let res = rx.await;
            this.update(cx, |this, cx| {
                match res {
                    Ok(Ok(d)) => {
                        this.request_thumb(&d.thumb);
                        if let View::Playlist { key: cur, detail, .. } = &mut this.view
                            && *cur == key
                        {
                            *detail = Some(d);
                        }
                    }
                    Ok(Err(e)) => this.say(format!("Couldn't load the playlist: {e:#}")),
                    Err(_) => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The loaded tracks of the playlist on screen.
    fn playlist_tracks_shown(&self) -> Option<(String, Vec<crate::plex::Track>)> {
        match &self.view {
            View::Playlist { title, detail: Some(d), .. } => Some((title.clone(), d.tracks.clone())),
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
        let Some((title, mut tracks)) = self.playlist_tracks_shown() else { return };
        if tracks.is_empty() {
            return;
        }
        if shuffle {
            crate::shuffle(&mut tracks);
            self.start_queue(format!("{title} (shuffled)"), None, tracks, 0, cx);
        } else {
            self.start_queue(title, None, tracks, i, cx);
        }
    }

    fn remove_playlist_item(&mut self, key: String, item_id: u64, title: String, cx: &mut Context<Self>) {
        let k = key.clone();
        let rx = crate::plex_call(move |c| async move { c.remove_from_playlist(&k, item_id).await });
        cx.spawn(async move |this, cx| {
            let res = rx.await;
            this.update(cx, |this, cx| {
                match res {
                    Ok(Ok(())) => {
                        this.say(format!("Removed \u{201c}{title}\u{201d}"));
                        this.load_playlist(key, cx);
                        this.reload_playlists(cx);
                    }
                    Ok(Err(e)) => this.say(format!("Couldn't remove \u{201c}{title}\u{201d}: {e:#}")),
                    Err(_) => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
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
                let more = if d.tracks.len() >= LIMIT {
                    format!(" · showing the first {LIMIT}")
                } else if (d.tracks.len() as u64) < n {
                    format!(" · {} unavailable (no file on the server)", n - d.tracks.len() as u64)
                } else {
                    String::new()
                };
                let len = if mins >= 60 { format!("{} h {} min", mins / 60, mins % 60) } else { format!("{mins} min") };
                format!("{n} tracks, {len}{more}")
            }
            None => listed.map(|p| format!("{} tracks · loading…", p.count)).unwrap_or_else(|| "Loading…".into()),
        };
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
                    .child(div().text_size(px(14.0)).text_color(rgb(t.text2)).child(facts)),
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
            );

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
            .child(div().w(px(64.0)).flex().justify_end().child("TIME"))
            .child(div().w(px(28.0)));

        let n = detail.map(|d| d.tracks.len()).unwrap_or(0);
        let list = uniform_list(
            "playlist-tracks",
            n,
            cx.processor(move |this, range: std::ops::Range<usize>, _window, cx| {
                let t = this.theme.clone();
                let View::Playlist { key, title, detail: Some(d) } = &this.view else { return Vec::new() };
                let (key, list_title, smart) = (key.clone(), title.clone(), d.smart);
                // The row playing now, when this playlist is what's playing.
                let playing_key = this
                    .now
                    .as_ref()
                    .filter(|n| n.context == list_title)
                    .and_then(|n| n.tracks.get(n.idx))
                    .map(|tr| tr.rating_key.clone());
                let rows: Vec<(usize, crate::plex::Track, u64)> = range.filter_map(|i| Some((i, d.tracks.get(i)?.clone(), d.item_ids.get(i).copied().unwrap_or(0)))).collect();
                rows.into_iter()
                    .map(|(i, tr, item_id)| {
                        this.request_thumb(&tr.thumb);
                        let art = match this.art.get(&tr.thumb).cloned() {
                            Some(im) => img(im).size(px(36.0)).rounded(px(4.0)).object_fit(ObjectFit::Cover).into_any_element(),
                            None => gpui::img(this.mark(crate::art::placeholder_for(&tr.thumb))).size(px(36.0)).rounded(px(4.0)).object_fit(ObjectFit::Cover).into_any_element(),
                        };
                        let playing = playing_key.as_deref() == Some(tr.rating_key.as_str());
                        let (k, name) = (key.clone(), tr.title.clone());
                        let row = div()
                            .id(("pl-track", i))
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
                            .child(
                                div()
                                    .w(px(40.0))
                                    .font_family(MONO)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_size(px(13.0))
                                    .text_color(rgb(if playing { t.accent } else { t.muted }))
                                    .child(if playing { "▶".to_string() } else { (i + 1).to_string() }),
                            )
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
                    .when(detail.is_some_and(|d| d.tracks.is_empty()), |d| {
                        d.child(div().px(px(PAD_X + 12.0)).pt(px(12.0)).text_size(px(13.0)).text_color(rgb(t.muted)).child("This playlist is empty."))
                    })
                    .child(list),
            )
            .into_any_element()
    }
}
