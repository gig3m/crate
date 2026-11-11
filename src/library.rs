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
        if self.lib_sort == LibSort::Added && self.lib_filters.is_empty() {
            self.lib_view = None;
            cx.notify();
            return;
        }
        let gen_id = self.lib_gen;
        let section = std::env::var("PLEX_SECTION").unwrap_or_else(|_| "5".into());
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
                None => div().size(px(w)).rounded(px(8.0)).bg(rgb(t.raised)).into_any_element(),
            };
            cards.push(
                div()
                    .id(cell)
                    .w(px(w))
                    .flex()
                    .flex_col()
                    .gap(px(7.0))
                    .cursor_pointer()
                    .child(cover)
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
                    .on_click(cx.listener(move |this, _, _, cx| this.open_album(ix, cx))),
            );
        }
        div().h(px(self.row_h())).pl(px(PAD_X)).flex().gap(px(GRID_GAP_X)).children(cards)
    }
}
