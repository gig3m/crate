//! Home from the design canvas (Main.dc.html): jump-back-in hero, radio
//! cards, stations, mood/decade chips, then the server's album hubs.

use gpui::{Context, FontWeight, ObjectFit, div, img, linear_color_stop, linear_gradient, prelude::*, px, rgb};

use crate::{Aulos, PAD_X, View, icons, plex::Album, shell::MONO, theme::{Theme, mix}};

pub(crate) fn ago(ts: i64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(ts);
    let s = (now - ts).max(0);
    match s {
        0..=89 => "just now".into(),
        90..=3599 => format!("{} min ago", s / 60),
        3600..=86_399 => format!("{} h ago", s / 3600),
        _ => format!("{} d ago", s / 86_400),
    }
}

/// One-line descriptions for Plex's built-in stations.
pub(crate) fn station_blurb(title: &str) -> &'static str {
    match title {
        "Library Radio" => "Your whole library, mixed",
        "Deep Cuts Radio" => "Album tracks you rarely play",
        "Time Travel Radio" => "Oldest to newest, one flow",
        "Random Album Radio" => "A whole album, then another",
        _ => "Plex station",
    }
}

impl Aulos {
    /// Start a Plex station (`/library/sections/N/stations/M`), radio key, or
    /// `query:<filter>` random radio; it refills as it runs low.
    pub(crate) fn play_station(&mut self, key: String, title: String, cx: &mut Context<Self>) {
        self.status = format!("Starting {title}…").into();
        cx.notify();
        self.play_intent += 1;
        let intent = self.play_intent;
        let rx = crate::fetch_more(key.clone());
        cx.spawn(async move |this, cx| {
            let res = rx.await;
            this.update(cx, |this, cx| match res {
                // Something else was played since the click: drop this.
                _ if this.play_intent != intent => {}
                Ok(Ok(tracks)) if !tracks.is_empty() => {
                    this.start_queue(title, None, tracks, 0, cx);
                    if let Some(n) = this.now.as_mut() {
                        n.refill = Some(key);
                    }
                }
                Ok(Ok(_)) => this.status = format!("{title} returned no tracks").into(),
                Ok(Err(e)) => this.status = format!("{title}: {e:#}").into(),
                Err(_) => {}
            })
            .ok();
        })
        .detach();
    }

    /// Play a playlist from the top (first 500 tracks).
    pub(crate) fn play_playlist(&mut self, key: String, title: String, cx: &mut Context<Self>) {
        self.play_intent += 1;
        let intent = self.play_intent;
        let rx = crate::plex_call(move |c| async move { c.playlist_tracks(&key, 500).await });
        cx.spawn(async move |this, cx| {
            let res = rx.await;
            this.update(cx, |this, cx| match res {
                _ if this.play_intent != intent => {}
                Ok(Ok(tracks)) if !tracks.is_empty() => this.start_queue(title, None, tracks, 0, cx),
                Ok(Ok(_)) => this.status = format!("{title} is empty").into(),
                Ok(Err(e)) => this.status = format!("{title}: {e:#}").into(),
                Err(_) => {}
            })
            .ok();
        })
        .detach();
    }

    /// Show the album grid filtered by a Plex query such as `decade=1990`.
    pub(crate) fn filter_library(&mut self, kind: &'static str, filter: String, label: String, cx: &mut Context<Self>) {
        self.add_filter(crate::library::LibFilter { kind, label, query: filter }, cx);
    }

    fn cover(&self, thumb: &str, size: f32, round: f32, t: &Theme) -> gpui::AnyElement {
        match self.art.get(thumb).cloned() {
            Some(i) => img(i).size(px(size)).flex_none().rounded(px(round)).object_fit(ObjectFit::Cover).into_any_element(),
            None => div().size(px(size)).flex_none().rounded(px(round)).bg(rgb(t.raised)).into_any_element(),
        }
    }

    fn pill(id: &'static str, label: &'static str, t: &Theme) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .h(px(40.0))
            .px(px(16.0))
            .flex()
            .items_center()
            .rounded(px(20.0))
            .border_1()
            .border_color(rgb(t.line))
            .text_size(px(14.0))
            .font_weight(FontWeight::MEDIUM)
            .text_color(rgb(t.text))
            .cursor_pointer()
            .hover(|d| d.bg(rgb(t.raised)))
            .child(label)
    }

    fn hero(&self, t: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let home = self.home.as_ref();
        let last: Option<(Album, i64)> = home.and_then(|h| h.last_played.clone());
        let station = home.and_then(|h| h.rows.iter().find(|r| r.kind == "station")).and_then(|r| r.items.first()).cloned();
        let bg = linear_gradient(110.0, linear_color_stop(rgb(mix(t.surface, t.discovery, 0.14)), 0.0), linear_color_stop(rgb(t.surface), 0.6));
        let mut hero = div()
            .flex_1()
            .min_w_0()
            .rounded(px(14.0))
            .p(px(24.0))
            .flex()
            .gap(px(24.0))
            .items_end()
            .bg(bg)
            .border_1()
            .border_color(rgb(t.border));
        match last {
            Some((album, at)) => {
                let ix = self.albums.iter().position(|a| a.rating_key == album.rating_key);
                let sub = [album.artist.clone(), album.year.clone()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
                hero = hero.child(div().shadow_lg().child(self.cover(&album.thumb, 168.0, 6.0, t))).child(
                    div()
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
                                .child(format!("JUMP BACK IN · PLAYED {}", ago(at).to_uppercase())),
                        )
                        .child(div().text_size(px(34.0)).font_weight(FontWeight::BOLD).text_color(rgb(t.text_strong)).truncate().child(album.title.clone()))
                        .child(div().text_size(px(15.0)).text_color(rgb(t.text2)).child(sub))
                        .child(
                            div()
                                .mt(px(8.0))
                                .flex()
                                .gap(px(10.0))
                                .child(
                                    div()
                                        .id("hero-play")
                                        .h(px(40.0))
                                        .px(px(20.0))
                                        .flex()
                                        .items_center()
                                        .gap(px(8.0))
                                        .rounded(px(20.0))
                                        .bg(rgb(t.accent))
                                        .text_color(rgb(t.base))
                                        .text_size(px(14.0))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .cursor_pointer()
                                        .hover(|d| d.opacity(0.9))
                                        .child(icons::icon("play", 16.0, t.base))
                                        .child("Play")
                                        .when_some(ix, |d, ix| d.on_click(cx.listener(move |this, _, _, cx| this.play_album(ix, cx)))),
                                )
                                .when_some(ix, |d, ix| {
                                    d.child(Self::pill("hero-open", "Open album", t).on_click(cx.listener(move |this, _, _, cx| this.open_album(ix, cx))))
                                })
                                .when_some(station.clone(), |d, s| {
                                    d.child(Self::pill("hero-radio", "Library radio", t).on_click(cx.listener(move |this, _, _, cx| {
                                        this.play_station(s.key.clone(), s.title.clone(), cx)
                                    })))
                                }),
                        ),
                );
            }
            None => {
                hero = hero.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(10.0))
                        .child(div().font_family(MONO).text_size(px(11.0)).text_color(rgb(t.text2)).child("WELCOME"))
                        .child(div().text_size(px(34.0)).font_weight(FontWeight::BOLD).text_color(rgb(t.text_strong)).child("Your library")),
                );
            }
        }
        hero.into_any_element()
    }

    fn side_card(
        id: &'static str,
        title: &'static str,
        blurb: String,
        ic: &'static str,
        color: u32,
        t: &Theme,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .flex_1()
            .rounded(px(14.0))
            .p(px(18.0))
            .bg(rgb(t.raised))
            .border_1()
            .border_color(rgb(t.border))
            .flex()
            .flex_col()
            .justify_between()
            .gap(px(12.0))
            .cursor_pointer()
            .hover(|d| d.border_color(rgb(t.line)))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .child(div().text_size(px(16.0)).font_weight(FontWeight::SEMIBOLD).text_color(rgb(t.text_strong)).child(title))
                    .child(icons::icon(ic, 20.0, color)),
            )
            .child(div().text_size(px(13.0)).text_color(rgb(t.muted)).child(blurb))
    }

    fn section_head(title: &str, link: Option<gpui::AnyElement>, t: &Theme) -> gpui::Div {
        div()
            .flex()
            .items_baseline()
            .justify_between()
            .child(div().text_size(px(20.0)).font_weight(FontWeight::SEMIBOLD).text_color(rgb(t.text_strong)).child(title.to_string()))
            .children(link)
    }

    fn chip(id: (&'static str, usize), label: String, mono: bool, t: &Theme) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .h(px(30.0))
            .px(px(if mono { 10.0 } else { 12.0 }))
            .flex()
            .items_center()
            .rounded(px(if mono { 6.0 } else { 15.0 }))
            .border_1()
            .border_color(rgb(t.line))
            .text_color(rgb(t.text))
            .when(mono, |d| d.font_family(MONO).text_size(px(12.0)))
            .when(!mono, |d| d.text_size(px(13.0)))
            .font_weight(FontWeight::MEDIUM)
            .cursor_pointer()
            .hover(|d| d.bg(rgb(t.raised)))
            .child(label)
    }

    pub(crate) fn home_view(&self, t: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let col = div()
            .id("home")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px(px(PAD_X))
            .pt(px(4.0))
            .pb(px(48.0))
            .flex()
            .flex_col()
            .gap(px(36.0));
        let Some(home) = self.home.as_ref() else {
            return col.child(div().text_color(rgb(t.muted)).child("Loading…")).into_any_element();
        };

        // Hero + the two radio cards.
        let library_radio = home.rows.iter().find(|r| r.kind == "station").and_then(|r| r.items.first()).cloned();
        let right = div()
            .w(px(320.0))
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(16.0))
            .child({
                let c = Self::side_card(
                    "card-radio",
                    "Library Radio",
                    "Your library, mixed by Plex: most-played next to what you haven't heard in a while".into(),
                    "stations",
                    t.accent,
                    t,
                );
                match library_radio {
                    Some(s) => c.on_click(cx.listener(move |this, _, _, cx| this.play_station(s.key.clone(), s.title.clone(), cx))),
                    None => c,
                }
            })
            .child(
                Self::side_card(
                    "card-sonic",
                    "Sonic Adventure",
                    "Pick two tracks. Aulos walks the sonic path between them.".into(),
                    "sonic",
                    t.discovery,
                    t,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.go(View::Soon("stations"));
                    cx.notify();
                })),
            );
        let top = div().flex().gap(px(16.0)).child(self.hero(t, cx)).child(right);

        // Stations.
        let swatches = [t.accent, t.discovery, t.healthy, t.text2];
        let stations: Vec<_> = home
            .rows
            .iter()
            .filter(|r| r.kind == "station")
            .flat_map(|r| r.items.iter())
            .take(4)
            .enumerate()
            .map(|(i, s)| {
                let (key, title) = (s.key.clone(), s.title.clone());
                let c = swatches[i % swatches.len()];
                div()
                    .id(("station", i))
                    .flex_1()
                    .min_w_0()
                    .h(px(76.0))
                    .px(px(16.0))
                    .flex()
                    .items_center()
                    .gap(px(14.0))
                    .rounded(px(12.0))
                    .border_1()
                    .border_color(rgb(t.border))
                    .bg(rgb(t.surface))
                    .cursor_pointer()
                    .hover(|d| d.bg(rgb(t.raised)))
                    .child(
                        div()
                            .size(px(44.0))
                            .flex_none()
                            .rounded(px(22.0))
                            .bg(linear_gradient(135.0, linear_color_stop(rgb(c), 0.0), linear_color_stop(rgb(mix(c, t.base, 0.7)), 1.0)))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icons::icon("stations", 18.0, t.base)),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(3.0))
                            .child(div().text_size(px(14.0)).font_weight(FontWeight::SEMIBOLD).text_color(rgb(t.text)).truncate().child(s.title.clone()))
                            .child(div().text_size(px(12.0)).text_color(rgb(t.muted)).truncate().child(station_blurb(&s.title))),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.play_station(key.clone(), title.clone(), cx)))
            })
            .collect();
        let label = |s: &'static str| {
            div().font_family(MONO).font_weight(FontWeight::MEDIUM).text_size(px(11.0)).text_color(rgb(t.muted)).mr(px(4.0)).child(s)
        };
        let moods: Vec<_> = home
            .moods
            .iter()
            .enumerate()
            .map(|(i, (f, l))| {
                let (f, l) = (f.clone(), l.clone());
                Self::chip(("mood", i), l.clone(), false, t).on_click(cx.listener(move |this, _, _, cx| this.filter_library("Mood", f.clone(), l.clone(), cx)))
            })
            .collect();
        let decades: Vec<_> = home
            .decades
            .iter()
            .enumerate()
            .map(|(i, (f, l))| {
                let (f, l) = (f.clone(), l.clone());
                Self::chip(("decade", i), l.clone(), true, t)
                    .on_click(cx.listener(move |this, _, _, cx| this.filter_library("Decade", f.clone(), l.clone(), cx)))
            })
            .collect();
        let all_stations = div()
            .id("all-stations")
            .text_size(px(13.0))
            .text_color(rgb(t.text2))
            .cursor_pointer()
            .hover(|d| d.text_color(rgb(t.text_strong)))
            .child("All stations")
            .on_click(cx.listener(|this, _, _, cx| {
                this.go(View::Soon("stations"));
                cx.notify();
            }))
            .into_any_element();
        let stations_section = div()
            .flex()
            .flex_col()
            .gap(px(14.0))
            .child(Self::section_head("Stations", Some(all_stations), t))
            .child(div().flex().gap(px(12.0)).children(stations))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(8.0))
                    .when(!home.moods.is_empty(), |d| d.child(label("MOOD")).children(moods).child(div().w(px(16.0))))
                    .when(!home.decades.is_empty(), |d| d.child(label("DECADE")).children(decades)),
            );

        // The server's album/artist hubs, one row each, as many cards as fit.
        let cols = self.home_cols();
        let card_w = self.home_card_w(cols);
        let mut col = col.child(top).child(stations_section);
        for (ri, row) in home.rows.iter().enumerate().filter(|(_, r)| r.kind != "station") {
            let artist = row.kind == "artist";
            let cards: Vec<_> = row
                .items
                .iter()
                .take(cols)
                .enumerate()
                .map(|(ci, it)| {
                    let (kind, key, title) = (row.kind.clone(), it.rating_key.clone(), it.title.clone());
                    div()
                        .id(("hub", ri * 100 + ci))
                        .w(px(card_w))
                        .flex_none()
                        .flex()
                        .flex_col()
                        .gap(px(8.0))
                        .cursor_pointer()
                        .child(self.cover(&it.thumb, card_w, if artist { card_w / 2.0 } else { 8.0 }, t))
                        .child(
                            div()
                                .text_size(px(14.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(t.text_strong))
                                .truncate()
                                .when(artist, |d| d.flex().justify_center())
                                .child(it.title.clone()),
                        )
                        .when(!it.subtitle.is_empty(), |d| d.child(div().text_size(px(12.0)).text_color(rgb(t.muted)).truncate().child(it.subtitle.clone())))
                        .on_click(cx.listener(move |this, _, _, cx| this.open_hub_item(&kind, &key, &title, cx)))
                })
                .collect();
            col = col.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(14.0))
                    .child(Self::section_head(&row.title, None, t))
                    .child(div().flex().gap(px(20.0)).children(cards)),
            );
        }
        col.into_any_element()
    }

    /// Home grids: six columns at the canvas width, fewer when narrow.
    fn home_cols(&self) -> usize {
        let w = self.content_w;
        ((w + 20.0) / (150.0 + 20.0)).floor().clamp(2.0, 6.0) as usize
    }

    fn home_card_w(&self, cols: usize) -> f32 {
        ((self.content_w - 20.0 * (cols as f32 - 1.0)) / cols as f32).floor()
    }
}
