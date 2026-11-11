//! Sidebar library pages: Artists, Tracks, Playlists, Genres & Moods, History.

use gpui::{Context, FontWeight, ObjectFit, div, img, prelude::*, px, rgb, uniform_list};

use crate::{
    Aulos, PAD_X, fmt_ms,
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

impl Aulos {
    /// Data a page needs when it opens.
    pub(crate) fn load_page(&mut self, id: &str, cx: &mut Context<Self>) {
        let section = std::env::var("PLEX_SECTION").unwrap_or_else(|_| "5".into());
        match id {
            "history" => {
                let rx = crate::plex_call(move |c| async move { c.history(&section, 200).await });
                cx.spawn(async move |this, cx| {
                    if let Ok(Ok(h)) = rx.await {
                        this.update(cx, |this, cx| {
                            let thumbs: Vec<String> = h.iter().take(40).map(|x| x.thumb.clone()).collect();
                            for th in thumbs {
                                this.request_thumb(&th);
                            }
                            this.history = Some(h);
                            cx.notify();
                        })
                        .ok();
                    }
                })
                .detach();
            }
            "tracks" => {
                let rx = crate::plex_call(move |c| async move { c.top_tracks(&section, 300).await });
                cx.spawn(async move |this, cx| {
                    if let Ok(Ok(v)) = rx.await {
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
                                    None => div().size(px(w)).rounded(px(w / 2.0)).bg(rgb(t.raised)).into_any_element(),
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
            .child(title("Artists", format!("{n} artists"), t))
            .child(list)
            .into_any_element()
    }

    fn track_row(&self, i: usize, tr: &Track, right: String, t: &Theme) -> gpui::Stateful<gpui::Div> {
        let art = match self.art.get(&tr.thumb).cloned() {
            Some(im) => img(im).size(px(40.0)).flex_none().rounded(px(4.0)).object_fit(ObjectFit::Cover).into_any_element(),
            None => div().size(px(40.0)).flex_none().rounded(px(4.0)).bg(rgb(t.raised)).into_any_element(),
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
            .child(div().font_family(MONO).text_size(px(12.0)).text_color(rgb(t.text2)).child(right))
    }

    pub(crate) fn tracks_view(&self, t: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(top) = self.top_tracks.clone() else {
            return title("Tracks", "loading…".into(), t).into_any_element();
        };
        let all: Vec<Track> = top.iter().map(|x| x.0.clone()).collect();
        let rows: Vec<_> = top
            .iter()
            .enumerate()
            .map(|(i, (tr, plays))| {
                let all = all.clone();
                self.track_row(i, tr, format!("{plays} plays · {}", fmt_ms(tr.duration_ms)), t)
                    .on_click(cx.listener(move |this, _, _, cx| this.start_queue("Most played".into(), None, all.clone(), i, cx)))
            })
            .collect();
        div()
            .id("tracks")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .child(title("Tracks", format!("your {} most played", top.len()), t))
            .child(div().px(px(PAD_X)).pb(px(40.0)).flex().flex_col().children(rows))
            .into_any_element()
    }

    pub(crate) fn history_view(&self, t: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(h) = self.history.clone() else {
            return title("History", "loading…".into(), t).into_any_element();
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
                };
                let (ak, tk) = (it.album_key.clone(), it.rating_key.clone());
                self.track_row(i, &tr, crate::home::ago(it.viewed_at), t)
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
                    None => div().size(px(w)).rounded(px(8.0)).bg(rgb(t.raised)).into_any_element(),
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
                    .on_click(cx.listener(move |this, _, _, cx| this.play_playlist(k.clone(), n.clone(), cx)))
            })
            .collect();
        div()
            .id("playlists")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .child(title("Playlists", format!("{} playlists · click to play", lists.len()), t))
            .child(div().px(px(PAD_X)).pb(px(40.0)).flex().flex_wrap().gap_x(px(GRID_GAP_X)).gap_y(px(22.0)).children(cards))
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
