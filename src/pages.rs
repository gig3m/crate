//! Sidebar library pages: Artists, Tracks, Playlists, Genres & Moods, History.

use gpui::{Context, FontWeight, ObjectFit, div, img, prelude::*, px, rgb, uniform_list};

use crate::{
    Crate, PAD_X, fmt_ms,
    library::{GRID_GAP_X, LibFilter},
    plex::{HistoryItem, Track},
    shell::MONO,
    theme::Theme,
};

fn title(s: &'static str, sub: String, t: &Theme) -> gpui::Div {
    div()
        .px(px(PAD_X))
        .pt(px(4.0))
        .pb(px(18.0))
        .flex()
        .items_baseline()
        .gap(px(14.0))
        .child(div().text_size(px(30.0)).font_weight(FontWeight::BOLD).text_color(rgb(t.text_strong)).child(s))
        .child(div().font_family(MONO).text_size(px(13.0)).text_color(rgb(t.muted)).child(sub))
}

impl Crate {
    /// Data a page needs when it opens.
    pub(crate) fn load_page(&mut self, id: &str, cx: &mut Context<Self>) {
        let section = crate::section();
        match id {
            "history" => {
                self.page_error.remove("history");
                let rx = crate::plex_call(move |c| async move { c.history(&section, 200).await });
                cx.spawn(async move |this, cx| {
                    let res = rx.await;
                    if let Ok(Err(e)) = &res {
                        let e = format!("{e:#}");
                        this.update(cx, |this, cx| {
                            this.page_error.insert("history", e);
                            cx.notify();
                        })
                        .ok();
                    }
                    if let Ok(Ok(h)) = res {
                        this.update(cx, |this, cx| {
                            let thumbs: Vec<String> = h.iter().take(40).map(|x| x.thumb.clone()).collect();
                            for th in thumbs {
                                this.request_thumb(&th);
                            }
                            // History doesn't carry ratings: fetch the ones not known yet.
                            let mut keys: Vec<String> = Vec::new();
                            for it in &h {
                                if !it.rating_key.is_empty() && !this.ratings.contains_key(&it.rating_key) && !keys.contains(&it.rating_key) {
                                    keys.push(it.rating_key.clone());
                                }
                            }
                            this.history = Some(h);
                            this.fetch_ratings(keys, cx);
                            cx.notify();
                        })
                        .ok();
                    }
                })
                .detach();
            }
            "tracks" => {
                self.page_error.remove("tracks");
                let rx = crate::plex_call(move |c| async move { c.top_tracks(&section, 300).await });
                cx.spawn(async move |this, cx| {
                    let res = rx.await;
                    if let Ok(Err(e)) = &res {
                        let e = format!("{e:#}");
                        this.update(cx, |this, cx| {
                            this.page_error.insert("tracks", e);
                            cx.notify();
                        })
                        .ok();
                    }
                    if let Ok(Ok(v)) = res {
                        this.update(cx, |this, cx| {
                            let thumbs: Vec<String> = v.iter().take(40).map(|x| x.0.thumb.clone()).collect();
                            for th in thumbs {
                                this.request_thumb(&th);
                            }
                            this.top_tracks = Some(v);
                            cx.notify();
                        })
                        .ok();
                    }
                })
                .detach();
            }
            "playlists" => {
                let thumbs: Vec<String> = self.home.as_ref().map(|h| h.playlists.iter().map(|p| p.thumb.clone()).collect()).unwrap_or_default();
                for th in thumbs {
                    self.request_thumb(&th);
                }
            }
            _ => {}
        }
    }

    /// Artists in the library (from the album list), sorted by name.
    fn artists(&self) -> Vec<(String, String, usize)> {
        let mut m: std::collections::HashMap<&str, (&str, usize)> = std::collections::HashMap::new();
        for a in &self.albums {
            if !a.artist_key.is_empty() {
                m.entry(a.artist_key.as_str()).or_insert((a.artist.as_str(), 0)).1 += 1;
            }
        }
        let mut v: Vec<(String, String, usize)> = m.into_iter().map(|(k, (n, c))| (k.to_string(), n.to_string(), c)).collect();
        v.sort_by_key(|a| a.1.to_lowercase().trim_start_matches("the ").to_string());
        v
    }

    pub(crate) fn artists_view(&mut self, t: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        if self.artist_cache_for != self.albums.len() {
            self.artist_cache = self.artists();
            self.artist_cache_for = self.albums.len();
        }
        let n = self.artist_cache.len();
        let cols = self.cols.max(1);
        let rows = n.div_ceil(cols);
        let list = uniform_list(
            "artists",
            rows,
            cx.processor(move |this, range: std::ops::Range<usize>, _window, cx| {
                let t = this.theme.clone();
                let w = this.card_w;
                range
                    .map(|r| {
                        let row: Vec<(String, String, usize)> = (0..cols).filter_map(|c| this.artist_cache.get(r * cols + c).cloned()).collect();
                        let cards: Vec<_> = row
                            .into_iter()
                            .map(|(key, name, count)| {
                                let thumb = format!("/library/metadata/{key}/thumb");
                                this.request_thumb(&thumb);
                                let face = match this.art.get(&thumb).cloned() {
                                    Some(im) => img(im).size(px(w)).rounded(px(w / 2.0)).object_fit(ObjectFit::Cover).into_any_element(),
                                    None => gpui::img(this.mark(crate::art::placeholder_for(&thumb))).size(px(w)).rounded(px(w / 2.0)).object_fit(gpui::ObjectFit::Cover).into_any_element(),
                                };
                                let (k2, n2) = (key.clone(), name.clone());
                                div()
                                    .id(gpui::SharedString::from(format!("artist-{key}")))
                                    .w(px(w))
                                    .flex()
                                    .flex_col()
                                    .items_center()
                                    .gap(px(7.0))
                                    .cursor_pointer()
                                    .child(face)
                                    .child(div().w_full().text_center().text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).text_color(rgb(t.text_strong)).truncate().child(name))
                                    .child(div().font_family(MONO).text_size(px(11.0)).text_color(rgb(t.muted)).child(if count == 1 { "1 album".to_string() } else { format!("{count} albums") }))
                                    .on_click(cx.listener(move |this, _, _, cx| this.open_artist(k2.clone(), n2.clone(), cx)))
                            })
                            .collect();
                        div().h(px(this.row_h() + 4.0)).pl(px(PAD_X)).flex().gap(px(GRID_GAP_X)).children(cards)
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.artist_scroll)
        .flex_1();
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(title("Artists", format!("{n} album artists"), t))
            .child(list)
            .into_any_element()
    }

    fn track_row(&self, i: usize, tr: &Track, right: String, t: &Theme, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let art = match self.art.get(&tr.thumb).cloned() {
            Some(im) => img(im).size(px(40.0)).flex_none().rounded(px(4.0)).object_fit(ObjectFit::Cover).into_any_element(),
            None => gpui::img(self.mark(crate::art::placeholder_for(&tr.thumb))).size(px(40.0)).flex_none().rounded(px(4.0)).object_fit(gpui::ObjectFit::Cover).into_any_element(),
        };
        let playing = self.now.as_ref().is_some_and(|n| n.tracks.get(n.idx).is_some_and(|x| x.rating_key == tr.rating_key));
        div()
            .id(("trow", i))
            .h(px(52.0))
            .px(px(12.0))
            .flex()
            .items_center()
            .gap(px(14.0))
            .rounded(px(8.0))
            .cursor_pointer()
            .when(playing, |d| d.bg(rgb(t.raised)))
            .hover(|d| d.bg(rgb(t.raised)))
            .child(div().w(px(32.0)).font_family(MONO).text_size(px(12.0)).text_color(rgb(if playing { t.accent } else { t.muted })).child((i + 1).to_string()))
            .child(art)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(div().text_size(px(14.0)).font_weight(FontWeight::MEDIUM).text_color(rgb(if playing { t.accent } else { t.text })).truncate().child(tr.title.clone()))
                    .child(div().text_size(px(12.0)).text_color(rgb(t.muted)).truncate().child(format!("{} · {}", tr.artist, tr.album_title))),
            )
            .child(self.track_stars(format!("trow-r-{i}"), tr, t, cx))
            .child(div().w(px(140.0)).flex_none().flex().justify_end().whitespace_nowrap().font_family(MONO).text_size(px(12.0)).text_color(rgb(t.text2)).child(right))
    }

    /// A page still loading, or (with Retry) one whose load failed.
    fn page_waiting(&self, name: &'static str, id: &'static str, t: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(err) = self.page_error.get(id).cloned() else {
            return title(name, "loading…".into(), t).into_any_element();
        };
        div()
            .flex()
            .flex_col()
            .child(title(name, "couldn't load".into(), t))
            .child(
                div()
                    .px(px(PAD_X))
                    .flex()
                    .flex_col()
                    .gap(px(12.0))
                    .child(div().text_size(px(13.0)).text_color(rgb(t.text2)).child(err))
                    .child(
                        div()
                            .id(("page-retry", id.len()))
                            .w(px(96.0))
                            .h(px(34.0))
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
                                this.load_page(id, cx);
                                cx.notify();
                            })),
                    ),
            )
            .into_any_element()
    }

    pub(crate) fn tracks_view(&self, t: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(top) = self.top_tracks.as_ref() else {
            return self.page_waiting("Most played", "tracks", t, cx);
        };
        let rows: Vec<_> = top
            .iter()
            .enumerate()
            .map(|(i, (tr, plays))| {
                // The list is read at click time, not copied into every row.
                self.track_row(i, tr, format!("{plays} plays · {}", fmt_ms(tr.duration_ms)), t, cx).on_click(cx.listener(move |this, _, _, cx| {
                    let all: Vec<Track> = this.top_tracks.as_ref().map(|v| v.iter().map(|x| x.0.clone()).collect()).unwrap_or_default();
                    if i < all.len() {
                        this.start_queue("Most played".into(), None, all, i, cx);
                    }
                }))
            })
            .collect();
        div()
            .id("tracks")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .child(title("Most played", format!("your top {} tracks", top.len()), t))
            .child(div().px(px(PAD_X)).pb(px(40.0)).flex().flex_col().children(rows))
            .into_any_element()
    }

    pub(crate) fn history_view(&self, t: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(h) = self.history.as_ref() else {
            return self.page_waiting("History", "history", t, cx);
        };
        let rows: Vec<_> = h
            .iter()
            .enumerate()
            .map(|(i, it): (usize, &HistoryItem)| {
                let tr = Track {
                    rating_key: it.rating_key.clone(),
                    title: it.title.clone(),
                    artist: it.artist.clone(),
                    duration_ms: 0,
                    part_key: String::new(),
                    album_key: it.album_key.clone(),
                    album_title: it.album.clone(),
                    thumb: it.thumb.clone(),
                    ..Default::default()
                };
                let (ak, tk) = (it.album_key.clone(), it.rating_key.clone());
                self.track_row(i, &tr, crate::home::ago(it.viewed_at), t, cx)
                    .on_click(cx.listener(move |this, _, _, cx| this.play_track_in_album(&ak, tk.clone(), cx)))
            })
            .collect();
        div()
            .id("history")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .child(title("History", "plays from every client on your server".into(), t))
            .child(div().px(px(PAD_X)).pb(px(40.0)).flex().flex_col().children(rows))
            .into_any_element()
    }

    pub(crate) fn playlists_view(&self, t: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let lists = self.home.as_ref().map(|h| h.playlists.clone()).unwrap_or_default();
        let w = self.card_w;
        let cards: Vec<_> = lists
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let cover = match self.art.get(&p.thumb).cloned() {
                    Some(im) => img(im).size(px(w)).rounded(px(8.0)).object_fit(ObjectFit::Cover).into_any_element(),
                    None => gpui::img(self.mark(crate::art::placeholder_for(&p.thumb))).size(px(w)).rounded(px(8.0)).object_fit(gpui::ObjectFit::Cover).into_any_element(),
                };
                let (k, n) = (p.rating_key.clone(), p.title.clone());
                div()
                    .id(("plist", i))
                    .w(px(w))
                    .flex()
                    .flex_col()
                    .gap(px(7.0))
                    .cursor_pointer()
                    .child(cover)
                    .child(div().text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).text_color(rgb(t.text_strong)).truncate().child(p.title.clone()))
                    .child(
                        div()
                            .font_family(MONO)
                            .text_size(px(11.0))
                            .text_color(rgb(t.muted))
                            .child(format!("{}{} tracks", if p.smart { "smart · " } else { "" }, p.count)),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.open_playlist(k.clone(), n.clone(), cx)))
            })
            .collect();
        div()
            .id("playlists")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .child(title("Playlists", format!("{} playlists · click to open", lists.len()), t))
            .child(div().px(px(PAD_X)).pb(px(40.0)).flex().flex_wrap().gap_x(px(GRID_GAP_X)).gap_y(px(22.0)).children(cards))
            .into_any_element()
    }

    pub(crate) fn offline_view(&self, t: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        use crate::offline::fmt_bytes;
        let albums = self.offline.index.albums.clone();
        let sizes: Vec<u64> = albums.iter().map(|a| a.bytes).collect();
        let total: u64 = sizes.iter().sum();
        let rows: Vec<_> = albums
            .into_iter()
            .zip(sizes)
            .enumerate()
            .map(|(i, (a, size))| {
                let job = self.offline.job.as_ref().filter(|j| j.key == a.key);
                let queued = self.offline.queue.contains(&a.key);
                let state = match job {
                    Some(j) => format!("downloading {}/{}", j.done_tracks, j.total_tracks),
                    None if a.complete => format!("{} tracks · {}", a.tracks.len(), fmt_bytes(size)),
                    None if queued => "queued".into(),
                    None => a.failed.clone().map(|f| format!("failed: {f}")).unwrap_or_else(|| "download failed".into()),
                };
                let can_retry = job.is_none() && !a.complete && !queued;
                let rk = a.key.clone();
                let ix = self.albums.iter().position(|x| x.rating_key == a.key);
                let key = a.key.clone();
                div()
                    .id(("kept", i))
                    .h(px(64.0))
                    .px(px(12.0))
                    .flex()
                    .items_center()
                    .gap(px(14.0))
                    .rounded(px(8.0))
                    .hover(|d| d.bg(rgb(t.raised)))
                    .child(self.cover_or_mark(&a.thumb, &a.key, 48.0, 4.0))
                    .child(
                        div()
                            .id(("kept-open", i))
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .cursor_pointer()
                            .child(div().text_size(px(14.0)).font_weight(FontWeight::MEDIUM).text_color(rgb(t.text_strong)).truncate().child(a.title.clone()))
                            .child(div().text_size(px(12.0)).text_color(rgb(t.muted)).truncate().child(a.artist.clone()))
                            .when_some(ix, |d, ix| d.on_click(cx.listener(move |this, _, _, cx| this.open_album(ix, cx)))),
                    )
                    .child(
                        div()
                            .font_family(MONO)
                            .text_size(px(12.0))
                            .text_color(rgb(if a.complete { t.healthy } else if can_retry { t.red } else { t.muted }))
                            .max_w(px(360.0))
                            .truncate()
                            .child(state),
                    )
                    .when(can_retry, |d| {
                        d.child(
                            div()
                                .id(("kept-retry", i))
                                .px(px(10.0))
                                .h(px(28.0))
                                .flex()
                                .items_center()
                                .rounded(px(14.0))
                                .border_1()
                                .border_color(rgb(t.line))
                                .text_size(px(12.0))
                                .text_color(rgb(t.text2))
                                .cursor_pointer()
                                .hover(|d| d.bg(rgb(t.raised)))
                                .child("Retry")
                                .on_click(cx.listener(move |this, _, _, cx| this.retry_offline(&rk, cx))),
                        )
                    })
                    .child(
                        div()
                            .id(("kept-rm", i))
                            .px(px(10.0))
                            .h(px(28.0))
                            .flex()
                            .items_center()
                            .rounded(px(14.0))
                            .border_1()
                            .border_color(rgb(t.line))
                            .text_size(px(12.0))
                            .text_color(rgb(t.text2))
                            .cursor_pointer()
                            .hover(|d| d.bg(rgb(t.raised)))
                            .child("Remove")
                            .on_click(cx.listener(move |this, _, _, cx| this.remove_offline(&key, cx))),
                    )
            })
            .collect();
        let n = self.offline.index.albums.len();
        div()
            .id("offline")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .child(title("Offline", format!("{n} albums · {} on disk", fmt_bytes(total)), t))
            .when(n == 0, |d| {
                d.child(
                    div()
                        .px(px(PAD_X))
                        .text_size(px(14.0))
                        .text_color(rgb(t.muted))
                        .child("Albums you keep offline play from this computer, even without the server. Use the download button on an album page."),
                )
            })
            .child(div().px(px(PAD_X)).pb(px(40.0)).flex().flex_col().children(rows))
            .into_any_element()
    }

    pub(crate) fn genres_view(&self, t: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let home = self.home.as_ref();
        let section = |label: &'static str, kind: &'static str, items: Vec<(String, String)>, cx: &mut Context<Self>| {
            div()
                .flex()
                .flex_col()
                .gap(px(12.0))
                .child(div().text_size(px(18.0)).font_weight(FontWeight::SEMIBOLD).text_color(rgb(t.text_strong)).child(label))
                .child(div().flex().flex_wrap().gap(px(10.0)).children(items.into_iter().enumerate().map(|(i, (q, l))| {
                    div()
                        .id((kind, i))
                        .h(px(44.0))
                        .px(px(18.0))
                        .flex()
                        .items_center()
                        .rounded(px(10.0))
                        .border_1()
                        .border_color(rgb(t.border))
                        .bg(rgb(t.surface))
                        .text_size(px(14.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(rgb(t.text))
                        .cursor_pointer()
                        .hover(|d| d.bg(rgb(t.raised)))
                        .child(l.clone())
                        .on_click(cx.listener(move |this, _, _, cx| this.add_filter(LibFilter { kind, label: l.clone(), query: q.clone() }, cx)))
                })))
        };
        div()
            .id("genres")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .child(title("Genres & Moods", "pick one to filter your albums".into(), t))
            .child(
                div()
                    .px(px(PAD_X))
                    .pb(px(40.0))
                    .flex()
                    .flex_col()
                    .gap(px(28.0))
                    .child(section("Genres", "Genre", home.map(|h| h.genres.clone()).unwrap_or_default(), cx))
                    .child(section("Moods", "Mood", home.map(|h| h.moods.clone()).unwrap_or_default(), cx))
                    .child(section("Decades", "Decade", home.map(|h| h.decades.clone()).unwrap_or_default(), cx)),
            )
            .into_any_element()
    }
}
