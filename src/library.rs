//! Albums page from the design canvas (Library.dc.html): sort, filter chips,
//! a virtualized grid sized to seven columns, and an A–Z jump rail.

use gpui::{Context, FontWeight, ObjectFit, Window, div, img, prelude::*, px, rgb};
use gpui_component::{
    button::{Button, ButtonVariants},
    menu::{DropdownMenu, PopupMenuItem},
};

use crate::{Crate, PAD_X, icons, plex::ArtRequest, shell::MONO, theme::Theme};

pub(crate) const GRID_GAP_X: f32 = 18.0;
pub(crate) const GRID_GAP_Y: f32 = 22.0;
pub(crate) const RAIL_W: f32 = 28.0;
pub(crate) const LIST_ROW_H: f32 = 56.0;
/// Title and artist lines under a cover.
const CAPTION_H: f32 = 7.0 + 18.0 + 7.0 + 16.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum LibSort {
    Added,
    Artist,
    Title,
    Year,
    Plays,
    Rating,
    LastPlayed,
}

impl LibSort {
    pub(crate) const ALL: [LibSort; 7] =
        [LibSort::Added, LibSort::Artist, LibSort::Title, LibSort::Year, LibSort::Plays, LibSort::Rating, LibSort::LastPlayed];

    pub(crate) fn label(self) -> &'static str {
        match self {
            LibSort::Added => "Date added ↓",
            LibSort::Artist => "Artist",
            LibSort::Title => "Title",
            LibSort::Year => "Year ↓",
            LibSort::Plays => "Plays",
            LibSort::Rating => "Rating",
            LibSort::LastPlayed => "Last played",
        }
    }

    fn param(self) -> &'static str {
        match self {
            LibSort::Added => "sort=addedAt:desc",
            LibSort::Artist => "sort=artist.titleSort,titleSort",
            LibSort::Title => "sort=titleSort",
            LibSort::Year => "sort=year:desc",
            LibSort::Plays => "sort=viewCount:desc",
            LibSort::Rating => "sort=userRating:desc",
            LibSort::LastPlayed => "sort=lastViewedAt:desc",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LibFilter {
    /// "Genre", "Decade", "Mood", "Rating"
    pub kind: &'static str,
    pub label: String,
    /// Plex query fragment, e.g. `decade=1990`.
    pub query: String,
}

impl Crate {
    /// Re-query the album order for the current sort and filters. The default
    /// (date added, no filters) is the cached list as-is.
    pub(crate) fn refresh_library(&mut self, cx: &mut Context<Self>) {
        self.lib_gen += 1;
        self.scroll = gpui::UniformListScrollHandle::new();
        self.cursor = None;
        if self.lib_sort == LibSort::Added && self.lib_filters.is_empty() {
            self.lib_view = None;
            cx.notify();
            return;
        }
        let gen_id = self.lib_gen;
        let section = crate::section();
        let query = self.lib_filters.iter().map(|f| f.query.as_str()).chain(std::iter::once(self.lib_sort.param())).collect::<Vec<_>>().join("&");
        let rx = crate::plex_call(move |c| async move { c.album_keys_where(&section, &query).await });
        cx.spawn(async move |this, cx| {
            if let Ok(res) = rx.await {
                this.update(cx, |this, cx| {
                    if this.lib_gen != gen_id {
                        return;
                    }
                    match res {
                        Ok(keys) => {
                            let pos: std::collections::HashMap<&str, usize> =
                                this.albums.iter().enumerate().map(|(i, a)| (a.rating_key.as_str(), i)).collect();
                            let view = keys.iter().filter_map(|k| pos.get(k.as_str()).copied()).collect();
                            this.lib_view = Some(view);
                            // The order changed under the cursor.
                            this.cursor = None;
                        }
                        Err(e) => this.status = format!("Library: {e:#}").into(),
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// Add (or replace, per kind) a filter and show the album grid.
    pub(crate) fn add_filter(&mut self, f: LibFilter, cx: &mut Context<Self>) {
        self.lib_filters.retain(|x| x.kind != f.kind);
        self.lib_filters.push(f);
        // What's on screen changes: start a fresh selection.
        self.selection.clear();
        self.refresh_library(cx);
        if !matches!(self.view, crate::View::Library) {
            self.go(crate::View::Library);
        }
        cx.notify();
    }

    pub(crate) fn grid_layout(&mut self, content_w: f32) {
        let usable = (content_w - RAIL_W).max(200.0);
        let cols = ((usable + GRID_GAP_X) / (130.0 + GRID_GAP_X)).floor().clamp(2.0, 7.0) as usize;
        self.cols = cols;
        self.card_w = ((usable - GRID_GAP_X * (cols as f32 - 1.0)) / cols as f32).floor();
    }

    pub(crate) fn row_h(&self) -> f32 {
        self.card_w + CAPTION_H + GRID_GAP_Y
    }

    pub(crate) fn library_header(&self, t: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let this = cx.entity().downgrade();
        let sort = self.lib_sort;
        let sort_btn = Button::new("lib-sort")
            .outline()
            .label(format!("Sort  {}", sort.label()))
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu;
                for s in LibSort::ALL {
                    let this = this.clone();
                    menu = menu.item(PopupMenuItem::new(s.label()).checked(s == sort).on_click(move |_, _, cx| {
                        this.update(cx, |this, cx| {
                            this.lib_sort = s;
                            this.selection.clear();
                            this.refresh_library(cx);
                        })
                        .ok();
                    }));
                }
                menu
            });

        let home = self.home.as_ref();
        let decades: Vec<(String, String)> = home.map(|h| h.decades.clone()).unwrap_or_default();
        let moods: Vec<(String, String)> = home.map(|h| h.moods.clone()).unwrap_or_default();
        let genres: Vec<(String, String)> = home.map(|h| h.genres.clone()).unwrap_or_default();
        let this = cx.entity().downgrade();
        let add_btn = Button::new("lib-add-filter").ghost().label("+ Filter").dropdown_menu(move |menu, window, cx| {
            let pick = |kind: &'static str, items: Vec<(String, String)>, this: gpui::WeakEntity<Crate>| {
                move |mut m: gpui_component::menu::PopupMenu, _: &mut Window, _: &mut Context<gpui_component::menu::PopupMenu>| {
                    for (q, l) in &items {
                        let (q, l) = (q.clone(), l.clone());
                        let this = this.clone();
                        m = m.item(PopupMenuItem::new(l.clone()).on_click(move |_, _, cx| {
                            let f = LibFilter { kind, label: l.clone(), query: q.clone() };
                            this.update(cx, |this, cx| this.add_filter(f, cx)).ok();
                        }));
                    }
                    m
                }
            };
            let rating = vec![("userRating>>=8".to_string(), "≥ 4★".to_string()), ("userRating>>=10".to_string(), "5★".to_string())];
            menu.submenu("Genre", window, cx, pick("Genre", genres.clone(), this.clone()))
                .submenu("Decade", window, cx, pick("Decade", decades.clone(), this.clone()))
                .submenu("Mood", window, cx, pick("Mood", moods.clone(), this.clone()))
                .submenu("Rating", window, cx, pick("Rating", rating, this.clone()))
        });

        let chips: Vec<_> = self
            .lib_filters
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let kind = f.kind;
                div()
                    .h(px(30.0))
                    .pl(px(12.0))
                    .pr(px(6.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .rounded(px(15.0))
                    .bg(rgb(t.raised))
                    .border_1()
                    .border_color(rgb(t.line))
                    .text_size(px(13.0))
                    .child(div().text_color(rgb(t.muted)).child(f.kind))
                    .child(div().text_color(rgb(t.text_strong)).font_weight(FontWeight::MEDIUM).child(f.label.clone()))
                    .child(
                        div()
                            .id(("rm-filter", i))
                            .size(px(20.0))
                            .rounded(px(10.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(|d| d.bg(rgb(t.line)))
                            .child(icons::icon("close", 12.0, t.muted))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.lib_filters.retain(|x| x.kind != kind);
                                this.selection.clear();
                                this.refresh_library(cx);
                            })),
                    )
            })
            .collect();

        let count = match &self.lib_view {
            Some(v) if !self.lib_filters.is_empty() => format!("{} of {} match", v.len(), self.albums.len()),
            _ => format!("{} albums", self.albums.len()),
        };
        div()
            .px(px(PAD_X))
            .pt(px(4.0))
            .pb(px(18.0))
            .flex()
            .flex_col()
            .gap(px(18.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(14.0))
                    .child(div().text_size(px(30.0)).font_weight(FontWeight::BOLD).text_color(rgb(t.text_strong)).child("Albums"))
                    .child(div().font_family(MONO).text_size(px(13.0)).text_color(rgb(t.muted)).child(count))
                    .child(div().flex_1())
                    .child({
                        let toggle = |id: &'static str, ic: &'static str, on: bool, list: bool, cx: &mut Context<Self>| {
                            div()
                                .id(id)
                                .size(px(34.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .bg(rgb(if on { t.raised } else { t.surface }))
                                .cursor_pointer()
                                .child(icons::icon(ic, 16.0, if on { t.accent } else { t.text2 }))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.lib_list = list;
                                    this.scroll = gpui::UniformListScrollHandle::new();
                                    cx.notify();
                                }))
                        };
                        div()
                            .flex()
                            .rounded(px(8.0))
                            .border_1()
                            .border_color(rgb(t.border))
                            .overflow_hidden()
                            .child(toggle("view-grid", "grid", !self.lib_list, false, cx))
                            .child(toggle("view-list", "list", self.lib_list, true, cx))
                    })
                    .child(sort_btn),
            )
            .child(div().flex().flex_wrap().items_center().gap(px(8.0)).children(chips).child(add_btn))
            .into_any_element()
    }

    /// A–Z rail for artist/title sorts: jumps the grid to the first match.
    pub(crate) fn letter_rail(&self, t: &Theme, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let by_artist = match self.lib_sort {
            LibSort::Artist => true,
            LibSort::Title => false,
            _ => return None,
        };
        let order: Vec<usize> = match &self.lib_view {
            Some(v) => v.clone(),
            None => (0..self.albums.len()).collect(),
        };
        let bucket = |ix: usize| -> char {
            let Some(a) = self.albums.get(ix) else { return '#' };
            let s = if by_artist { &a.artist } else { &a.title };
            let c = s.trim_start_matches("The ").chars().next().unwrap_or('#').to_ascii_uppercase();
            if c.is_ascii_alphabetic() { c } else { '#' }
        };
        let mut first: std::collections::HashMap<char, usize> = std::collections::HashMap::new();
        for (cell, &ix) in order.iter().enumerate() {
            first.entry(bucket(ix)).or_insert(cell);
        }
        let cols = self.cols.max(1);
        let letters = std::iter::once('#').chain('A'..='Z');
        Some(
            div()
                .w(px(RAIL_W))
                .flex_none()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(1.0))
                .children(letters.map(|l| {
                    let target = first.get(&l).copied();
                    div()
                        .id(("letter", l as usize))
                        .font_family(MONO)
                        .font_weight(FontWeight::MEDIUM)
                        .text_size(px(10.0))
                        .line_height(px(19.0))
                        .text_color(rgb(if target.is_some() { t.muted } else { t.line }))
                        .when_some(target, |d, cell| {
                            d.cursor_pointer().hover(|d| d.text_color(rgb(t.accent))).on_click(cx.listener(move |this, _, _, cx| {
                                this.scroll.scroll_to_item(cell / cols, gpui::ScrollStrategy::Top);
                                cx.notify();
                            }))
                        })
                        .child(l.to_string())
                }))
                .into_any_element(),
        )
    }

    pub(crate) fn row(&mut self, row: usize, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let t = self.theme.clone();
        let w = self.card_w;
        let mut cards = Vec::with_capacity(self.cols);
        for c in 0..self.cols {
            let cell = row * self.cols + c;
            if cell >= self.cells() {
                break;
            }
            let ix = self.cell_album(cell);
            let Some(a) = self.album(ix).cloned() else { break };
            let art = self.art.get(&a.thumb).cloned();
            if art.is_none() && !a.thumb.is_empty() && self.requested.insert(a.thumb.clone()) {
                let _ = self.art_tx.send(ArtRequest { thumb: a.thumb.clone() });
            }
            let cover = match art {
                Some(image) => img(image).size(px(w)).rounded(px(8.0)).object_fit(ObjectFit::Cover).into_any_element(),
                None => gpui::img(self.mark(crate::art::placeholder_for(&a.thumb))).size(px(w)).rounded(px(8.0)).object_fit(gpui::ObjectFit::Cover).into_any_element(),
            };
            cards.push(
                div()
                    .id(cell)
                    .w(px(w))
                    .flex()
                    .flex_col()
                    .gap(px(7.0))
                    .cursor_pointer()
                    .child({
                        let kept = self.offline.index.get(&a.rating_key).is_some_and(|k| k.complete);
                        let selected = self.selection.contains(&a.rating_key);
                        let at_cursor = self.cursor == Some(cell);
                        div()
                            .relative()
                            .child(cover)
                            // Keyboard cursor: a ring just outside the cover, readable on light art.
                            .when(at_cursor, |d| {
                                d.child(div().absolute().top(px(-4.0)).left(px(-4.0)).right(px(-4.0)).bottom(px(-4.0)).rounded(px(11.0)).border_2().border_color(rgb(t.text_strong)))
                            })
                            .when(selected, |d| d.child(div().absolute().inset_0().rounded(px(8.0)).border_2().border_color(rgb(t.accent))))
                            .when(selected, |d| {
                                d.child(
                                    div()
                                        .absolute()
                                        .left(px(8.0))
                                        .top(px(8.0))
                                        .size(px(22.0))
                                        .rounded(px(11.0))
                                        .bg(rgb(t.accent))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(crate::icons::icon("check", 13.0, t.base)),
                                )
                            })
                            .when(kept, |d| {
                            d.child(
                                div()
                                    .absolute()
                                    .right(px(8.0))
                                    .bottom(px(8.0))
                                    .size(px(22.0))
                                    .rounded(px(11.0))
                                    .bg(gpui::hsla(240.0 / 360.0, 0.15, 0.08, 0.85))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(crate::icons::icon("offline", 12.0, t.healthy)),
                            )
                        })
                    })
                    .child(div().text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).text_color(rgb(t.text_strong)).truncate().child(a.title.clone()))
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .gap(px(6.0))
                            .text_size(px(12.0))
                            .text_color(rgb(t.muted))
                            .child(div().min_w_0().truncate().child(a.artist.clone()))
                            .child(div().flex_none().font_family(MONO).text_size(px(11.0)).child(a.year.clone())),
                    )
                    .on_click({
                        let key = a.rating_key.clone();
                        cx.listener(move |this, ev: &gpui::ClickEvent, _, cx| this.album_click(ix, &key, ev, cx))
                    }),
            );
        }
        div().h(px(self.row_h())).pl(px(PAD_X)).flex().gap(px(GRID_GAP_X)).children(cards)
    }

    /// Ctrl+click selects; once something is selected, a plain click does too.
    fn album_click(&mut self, ix: usize, key: &str, ev: &gpui::ClickEvent, cx: &mut Context<Self>) {
        let m = ev.modifiers();
        if m.control || m.platform || !self.selection.is_empty() {
            if let Some(p) = self.selection.iter().position(|k| k == key) {
                self.selection.remove(p);
            } else {
                self.selection.push(key.to_string());
            }
            cx.notify();
        } else {
            self.open_album(ix, cx);
        }
    }

    /// List view: one album per row.
    pub(crate) fn list_row(&mut self, cell: usize, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = self.theme.clone();
        let ix = self.cell_album(cell);
        let Some(a) = self.album(ix).cloned() else { return div().id(("lrow", cell)).h(px(LIST_ROW_H)).into_any_element() };
        self.request_thumb(&a.thumb.clone());
        let selected = self.selection.contains(&a.rating_key);
        let at_cursor = self.cursor == Some(cell);
        let kept = self.offline.index.get(&a.rating_key).is_some_and(|k| k.complete);
        let key = a.rating_key.clone();
        let row = div()
            .id(("lrow", cell))
            .w_full()
            .h(px(LIST_ROW_H))
            .px(px(10.0))
            .flex()
            .items_center()
            .gap(px(14.0))
            .rounded(px(8.0))
            .cursor_pointer()
            .border_1()
            .border_color(if selected { rgb(t.accent).into() } else if at_cursor { rgb(t.text2).into() } else { gpui::transparent_black() })
            .when(selected || at_cursor, |d| d.bg(rgb(t.raised)))
            .hover(|d| d.bg(rgb(t.raised)))
            .child(div().w(px(18.0)).flex().justify_center().when(selected, |d| d.child(crate::icons::icon("check", 14.0, t.accent))))
            .child(self.cover_or_mark(&a.thumb, &a.rating_key, 44.0, 4.0))
            .child(div().flex_1().min_w_0().text_size(px(14.0)).font_weight(FontWeight::MEDIUM).text_color(rgb(t.text_strong)).truncate().child(a.title.clone()))
            .child(div().w(px(260.0)).min_w_0().text_size(px(13.0)).text_color(rgb(t.text2)).truncate().child(a.artist.clone()))
            .child(div().w(px(56.0)).font_family(MONO).text_size(px(12.0)).text_color(rgb(t.muted)).child(a.year.clone()))
            .child(div().w(px(20.0)).when(kept, |d| d.child(crate::icons::icon("offline", 13.0, t.healthy))))
            .on_click(cx.listener(move |this, ev: &gpui::ClickEvent, _, cx| this.album_click(ix, &key, ev, cx)));
        div().w_full().px(px(PAD_X)).child(row).into_any_element()
    }

    /// The floating bar for a multi-selection (canvas).
    pub(crate) fn selection_bar(&self, t: &Theme, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if self.selection.is_empty() {
            return None;
        }
        let btn = |id: &'static str, label: &'static str, primary: bool| {
            div()
                .id(id)
                .h(px(34.0))
                .px(px(12.0))
                .flex()
                .items_center()
                .rounded(px(8.0))
                .text_size(px(13.0))
                .font_weight(if primary { FontWeight::SEMIBOLD } else { FontWeight::MEDIUM })
                .cursor_pointer()
                .when(primary, |d| d.bg(rgb(t.accent)).text_color(rgb(t.base)).hover(|d| d.opacity(0.9)))
                .when(!primary, |d| d.text_color(rgb(t.text)).hover(|d| d.bg(rgb(t.line))))
                .child(label)
        };
        let this = cx.entity().downgrade();
        let rate = Button::new("sel-rate").ghost().label("Rate…").dropdown_menu_with_anchor(gpui::Anchor::BottomLeft, move |mut menu, _, _| {
            for n in 1..=5u8 {
                let this = this.clone();
                menu = menu.item(PopupMenuItem::new("★".repeat(n as usize)).on_click(move |_, _, cx| {
                    this.update(cx, |this, cx| this.rate_selection(n, cx)).ok();
                }));
            }
            menu
        });
        Some(
            div()
                .absolute()
                .bottom(px(20.0))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    div()
                        .id("sel-bar")
                        // Clicks on the bar must not reach the album underneath (scrolling still does).
                        .block_mouse_except_scroll()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .pl(px(16.0))
                        .pr(px(8.0))
                        .py(px(8.0))
                        .rounded(px(14.0))
                        .bg(rgb(t.raised))
                        .border_1()
                        .border_color(rgb(t.line))
                        .shadow_2xl()
                        .child(div().mr(px(8.0)).text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).text_color(rgb(t.text_strong)).child(format!("{} selected", self.selection.len())))
                        .child(btn("sel-play", "Play", true).on_click(cx.listener(|this, _, _, cx| this.play_selection(false, cx))))
                        .child(btn("sel-shuffle", "Shuffle", false).on_click(cx.listener(|this, _, _, cx| this.play_selection(true, cx))))
                        .child(btn("sel-offline", "Keep offline", false).on_click(cx.listener(|this, _, _, cx| this.keep_selection_offline(cx))))
                        .child(rate)
                        .child(
                            div()
                                .id("sel-esc")
                                .px(px(8.0))
                                .font_family(MONO)
                                .text_size(px(11.0))
                                .text_color(rgb(t.muted))
                                .cursor_pointer()
                                .child("Esc")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.selection.clear();
                                    cx.notify();
                                })),
                        ),
                )
                .into_any_element(),
        )
    }

    /// The selected albums, in the order they were picked.
    fn selected_albums(&self) -> Vec<crate::plex::Album> {
        self.selection.iter().filter_map(|k| self.albums.iter().find(|a| &a.rating_key == k).cloned()).collect()
    }

    pub(crate) fn play_selection(&mut self, shuffle: bool, cx: &mut Context<Self>) {
        let albums = self.selected_albums();
        if albums.is_empty() {
            return;
        }
        self.play_intent += 1;
        let intent = self.play_intent;
        let keys: Vec<String> = albums.iter().map(|a| a.rating_key.clone()).collect();
        let name = if albums.len() == 1 { albums[0].title.clone() } else { format!("{} albums", albums.len()) };
        self.status = format!("Loading {name}…").into();
        cx.notify();
        // One album failing (deleted, timed out) skips that album, not the lot.
        let rx = crate::plex_call(move |c| async move {
            let mut all = Vec::new();
            let mut failed = 0;
            for k in keys {
                match c.tracks(&k).await {
                    Ok(t) => all.extend(t),
                    Err(_) => failed += 1,
                }
            }
            anyhow::ensure!(!all.is_empty() || failed == 0, "{failed} album(s) couldn't be loaded");
            Ok((all, failed))
        });
        cx.spawn(async move |this, cx| {
            let res = rx.await;
            this.update(cx, |this, cx| match res {
                _ if this.play_intent != intent => {}
                Ok(Ok((mut tracks, failed))) if !tracks.is_empty() => {
                    if shuffle {
                        crate::shuffle(&mut tracks);
                    }
                    this.selection.clear();
                    this.start_queue(name, None, tracks, 0, cx);
                    if failed > 0 {
                        this.status = format!("Skipped {failed} album(s) that couldn't be loaded").into();
                        cx.notify();
                    }
                }
                Ok(Ok(_)) => {
                    this.status = "Those albums have no tracks".into();
                    cx.notify();
                }
                Ok(Err(e)) => {
                    this.status = format!("Couldn't load the albums: {e:#}").into();
                    cx.notify();
                }
                Err(_) => {}
            })
            .ok();
        })
        .detach();
    }

    /// Fetches the albums' track lists on its own request (not the shared
    /// tracks channel), so opening an album isn't stuck behind a big batch.
    fn keep_selection_offline(&mut self, cx: &mut Context<Self>) {
        let albums: Vec<crate::plex::Album> = self.selected_albums().into_iter().filter(|a| self.offline.index.get(&a.rating_key).is_none()).collect();
        self.selection.clear();
        if albums.is_empty() {
            cx.notify();
            return;
        }
        let n = albums.len();
        self.status = format!("Keeping {n} album{} offline…", if n == 1 { "" } else { "s" }).into();
        cx.notify();
        let keys: Vec<String> = albums.iter().map(|a| a.rating_key.clone()).collect();
        let rx = crate::plex_call(move |c| async move {
            let mut out = Vec::new();
            for k in keys {
                if let Ok(t) = c.tracks(&k).await {
                    out.push((k, t));
                }
            }
            Ok(out)
        });
        cx.spawn(async move |this, cx| {
            let res = rx.await;
            this.update(cx, |this, cx| {
                let found = match res {
                    Ok(Ok(f)) => f,
                    _ => Vec::new(),
                };
                let failed = n - found.len();
                for (k, tracks) in found {
                    if let Some(a) = albums.iter().find(|a| a.rating_key == k).cloned() {
                        this.keep_offline(a, tracks, cx);
                    }
                }
                this.status = if failed > 0 {
                    format!("Couldn't load {failed} of {n} album(s) to keep offline").into()
                } else {
                    format!("Keeping {n} album{} offline: see Offline", if n == 1 { "" } else { "s" }).into()
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn rate_selection(&mut self, stars: u8, cx: &mut Context<Self>) {
        let albums = self.selected_albums();
        let n = albums.len();
        for album in albums {
            if let Some(Some(d)) = self.album_detail.get_mut(&album.rating_key) {
                d.rating = Some(f32::from(stars * 2));
            }
            self.rate_on_server(album.rating_key.clone(), Some(stars * 2), cx);
        }
        self.status = format!("Rated {n} album{} {}", if n == 1 { "" } else { "s" }, "★".repeat(stars as usize)).into();
        self.selection.clear();
        cx.notify();
    }
}
