//! App shell from the design canvas: sidebar and top bar.

use gpui::{Context, FontWeight, ObjectFit, Window, div, img, prelude::*, px, rgb};

use crate::{Aulos, PAD_X, SIDEBAR_W, View, icons, theme::Theme};

pub const MONO: &str = "JetBrainsMono Nerd Font";

/// Sidebar nav: (id, label, icon, key hint).
const NAV_PRIMARY: &[(&str, &str, &str, &str)] =
    &[("home", "Home", "home", "g h"), ("search", "Search", "search", "^K"), ("stations", "Stations", "stations", "g s")];
const NAV_LIBRARY: &[(&str, &str, &str, &str)] = &[
    ("artists", "Artists", "artists", ""),
    ("albums", "Albums", "albums", ""),
    ("tracks", "Tracks", "tracks", ""),
    ("playlists", "Playlists", "playlists", ""),
    ("genres", "Genres & Moods", "genres", ""),
    ("history", "History", "history", ""),
    ("offline", "Offline", "offline", ""),
];

impl Aulos {
    fn nav_active(&self) -> &'static str {
        match &self.view {
            View::Home => "home",
            View::Library | View::Album { .. } => "albums",
            View::Artist { .. } => "artists",
            View::Soon(id) => id,
        }
    }

    pub(crate) fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui::Focusable;
        let h = self.search.focus_handle(cx);
        window.focus(&h, cx);
    }

    fn nav_to(&mut self, id: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        let v = match id {
            "home" => View::Home,
            "albums" => {
                if !self.lib_filters.is_empty() {
                    self.lib_filters.clear();
                    self.refresh_library(cx);
                }
                View::Library
            }
            "search" => return self.open_palette(window, cx),
            other => View::Soon(other),
        };
        let same = match (&v, &self.view) {
            (View::Soon(a), View::Soon(b)) => a == b,
            (a, b) => std::mem::discriminant(a) == std::mem::discriminant(b),
        };
        if id == "stations" {
            self.refresh_sonic_progress(cx);
        }
        self.load_page(id, cx);
        if !same || id == "albums" {
            self.go(v);
        }
        cx.notify();
    }

    pub(crate) fn sidebar(&self, t: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let active = self.nav_active();
        let item = |id: &'static str, label: &'static str, ic: &'static str, key: &'static str, cx: &mut Context<Self>| {
            let on = id == active;
            div()
                .id(id)
                .h(px(36.0))
                .px(px(10.0))
                .flex()
                .items_center()
                .gap(px(12.0))
                .rounded(px(8.0))
                .text_size(px(14.0))
                .font_weight(FontWeight::MEDIUM)
                .cursor_pointer()
                .border_1()
                .when(on, |d| d.bg(rgb(t.raised)).text_color(rgb(t.accent)).border_color(rgb(t.line)))
                .when(!on, |d| d.border_color(gpui::transparent_black()).text_color(rgb(t.text2)).hover(|d| d.bg(rgb(t.raised))))
                .child(icons::icon(ic, 18.0, if on { t.accent } else { t.text2 }))
                .child(div().flex_1().child(label))
                .child(div().font_family(MONO).text_size(px(11.0)).text_color(rgb(t.muted)).child(key))
                .on_click(cx.listener(move |this, _, window, cx| this.nav_to(id, window, cx)))
        };
        let section = |label: &'static str| {
            div()
                .px(px(10.0))
                .pt(px(18.0))
                .pb(px(6.0))
                .font_family(MONO)
                .font_weight(FontWeight::MEDIUM)
                .text_size(px(11.0))
                .text_color(rgb(t.muted))
                .child(label.to_uppercase())
        };
        let swatch = [t.accent, t.healthy, t.discovery, t.text2];
        let playlists = self.home.as_ref().map(|h| h.playlists.iter().take(6).cloned().collect::<Vec<_>>()).unwrap_or_default();
        let pinned: Vec<_> = playlists
            .into_iter()
            .enumerate()
            .map(|(i, p)| {
                let tile = match self.art.get(&p.thumb).cloned() {
                    Some(im) => img(im).size(px(28.0)).flex_none().rounded(px(5.0)).object_fit(ObjectFit::Cover).into_any_element(),
                    None => div()
                        .size(px(28.0))
                        .flex_none()
                        .rounded(px(5.0))
                        .bg(rgb(swatch[i % swatch.len()]))
                        .flex()
                        .items_center()
                        .justify_center()
                        .when(p.smart, |d| d.child(icons::icon("bolt", 13.0, t.base)))
                        .into_any_element(),
                };
                div()
                    .id(("pin", i))
                    .px(px(10.0))
                    .py(px(6.0))
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .rounded(px(8.0))
                    .text_size(px(13.0))
                    .text_color(rgb(t.text2))
                    .cursor_pointer()
                    .hover(|d| d.bg(rgb(t.raised)))
                    .child(tile)
                    .child(div().flex_1().min_w_0().truncate().child(p.title.clone()))
                    .child(div().font_family(MONO).text_size(px(11.0)).text_color(rgb(t.muted)).child(p.count.to_string()))
                    .on_click(cx.listener(move |this, _, _, cx| this.play_playlist(p.rating_key.clone(), p.title.clone(), cx)))
            })
            .collect();
        let server = self.home.as_ref().map(|h| h.server.clone()).unwrap_or_else(|| "Plex".into());
        let online = self.home.is_some();

        let mut nav = div()
            .w(px(SIDEBAR_W))
            .h_full()
            .flex_none()
            .bg(rgb(t.surface))
            .border_r_1()
            .border_color(rgb(t.raised))
            .pt(px(18.0))
            .px(px(12.0))
            .pb(px(14.0))
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .px(px(10.0))
                    .pt(px(4.0))
                    .pb(px(18.0))
                    .child(
                        div()
                            .size(px(26.0))
                            .relative()
                            .child(icons::icon("logo", 26.0, t.accent).absolute())
                            .child(icons::icon("logo-bar", 26.0, t.text).absolute()),
                    )
                    .child(div().text_size(px(19.0)).font_weight(FontWeight::BOLD).text_color(rgb(t.text_strong)).child("aulos")),
            );
        for &(id, label, ic, key) in NAV_PRIMARY {
            nav = nav.child(item(id, label, ic, key, cx));
        }
        nav = nav.child(section("Library"));
        for &(id, label, ic, key) in NAV_LIBRARY {
            nav = nav.child(item(id, label, ic, key, cx));
        }
        nav.child(
            div()
                .flex()
                .items_end()
                .justify_between()
                .child(section("Pinned"))
                .child(div().size(px(24.0)).mr(px(6.0)).mb(px(4.0)).flex().items_center().justify_center().child(icons::icon("plus", 14.0, t.text2))),
        )
        .children(pinned)
        .child(div().flex_1())
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(10.0))
                .p(px(10.0))
                .rounded(px(10.0))
                .border_1()
                .border_color(rgb(t.border))
                .bg(rgb(t.base))
                .child(
                    div()
                        .size(px(30.0))
                        .flex_none()
                        .rounded(px(7.0))
                        .bg(rgb(t.raised))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(icons::icon("server", 16.0, if online { t.healthy } else { t.muted })),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(div().text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).text_color(rgb(t.text)).child(server))
                        .child(
                            div()
                                .font_family(MONO)
                                .text_size(px(11.0))
                                .text_color(rgb(t.muted))
                                .child(if online { "Music · direct play" } else { "connecting…" }),
                        ),
                )
                .child(icons::icon("updown", 14.0, t.muted)),
        )
        .into_any_element()
    }

    /// Back/forward, the search box, server pill and user chip.
    pub(crate) fn top_bar(&self, t: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let nav_btn = |id: &'static str, ic: &'static str, enabled: bool, cx: &mut Context<Self>| {
            div()
                .id(id)
                .size(px(36.0))
                .flex_none()
                .rounded(px(8.0))
                .border_1()
                .border_color(rgb(t.border))
                .bg(rgb(t.surface))
                .flex()
                .items_center()
                .justify_center()
                .when(enabled, |d| d.cursor_pointer().hover(|d| d.bg(rgb(t.raised))))
                .child(icons::icon(ic, 18.0, if enabled { t.text2 } else { t.line }))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if id == "nav-back" {
                        this.go_back(cx)
                    } else {
                        this.go_forward(cx)
                    }
                }))
        };
        let server = self.home.as_ref().map(|h| h.server.to_lowercase()).unwrap_or_else(|| "plex".into());
        div()
            .px(px(PAD_X))
            .pt(px(20.0))
            .pb(px(16.0))
            .flex()
            .items_center()
            .gap(px(12.0))
            .child(nav_btn("nav-back", "back", !self.back.is_empty(), cx))
            .child(nav_btn("nav-fwd", "forward", !self.fwd.is_empty(), cx))
            .child(
                div()
                    .id("search-trigger")
                    .flex_1()
                    .max_w(px(520.0))
                    .h(px(36.0))
                    .px(px(12.0))
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(rgb(t.border))
                    .bg(rgb(t.surface))
                    .cursor_pointer()
                    .hover(|d| d.border_color(rgb(t.line)))
                    .child(icons::icon("search", 16.0, t.muted))
                    .child(div().flex_1().text_size(px(14.0)).text_color(rgb(t.muted)).child("Search, play, or run a command"))
                    .child(
                        div()
                            .px(px(6.0))
                            .py(px(1.0))
                            .rounded(px(4.0))
                            .border_1()
                            .border_color(rgb(t.line))
                            .font_family(MONO)
                            .text_size(px(11.0))
                            .text_color(rgb(t.text2))
                            .child("Ctrl K"),
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.open_palette(window, cx))),
            )
            .child(div().flex_1())
            .child(
                div()
                    .h(px(36.0))
                    .px(px(12.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .rounded(px(8.0))
                    .bg(rgb(t.surface))
                    .border_1()
                    .border_color(rgb(t.border))
                    .font_family(MONO)
                    .font_weight(FontWeight::MEDIUM)
                    .text_size(px(12.0))
                    .text_color(rgb(t.text2))
                    .child(div().size(px(8.0)).rounded(px(4.0)).bg(rgb(if self.home.is_some() { t.healthy } else { t.muted })))
                    .child(format!("{server} · direct play")),
            )
            .into_any_element()
    }

    pub(crate) fn soon_view(&self, id: &'static str, t: &Theme) -> gpui::AnyElement {
        let label = NAV_PRIMARY.iter().chain(NAV_LIBRARY).find(|n| n.0 == id).map(|n| n.1).unwrap_or(id);
        div()
            .flex_1()
            .px(px(PAD_X))
            .pt(px(8.0))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(div().text_size(px(30.0)).font_weight(FontWeight::BOLD).text_color(rgb(t.text_strong)).child(label))
            .child(div().text_size(px(14.0)).text_color(rgb(t.muted)).child("This page is on the design canvas and not built yet."))
            .into_any_element()
    }
}
