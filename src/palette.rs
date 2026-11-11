//! Ctrl K command palette from the design canvas (Search.dc.html).

use gpui::{Context, FontWeight, KeyDownEvent, ObjectFit, Window, div, img, prelude::*, px, rgb};
use gpui_component::input::Input;

use crate::{Crate, icons, plex::{Playlist, SearchHit}, shell::MONO, theme::Theme};

const SCOPES: [&str; 5] = ["All", "Artists", "Albums", "Tracks", "Playlists"];

#[derive(Clone)]
pub(crate) enum Item {
    Hit(SearchHit),
    Playlist(Playlist),
    /// Plex radio seeded from an artist or album: (key, label).
    Radio(String, String),
    /// `#` scope: filter Albums by a genre, mood or decade.
    Filter { kind: &'static str, label: String, query: String },
    /// `>` scope: an app command.
    Command(PalCmd),
}

/// `>` commands (canvas: "> commands").
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum PalCmd {
    Nav(&'static str, &'static str),
    NowPlaying,
    Queue,
    Shuffle,
    Repeat,
    Settings,
    SonicAdventure,
    ThemeCrate,
    ThemeOmarchy,
}

impl PalCmd {
    const ALL: [PalCmd; 17] = [
        PalCmd::Nav("home", "Go to Home"),
        PalCmd::Nav("albums", "Go to Albums"),
        PalCmd::Nav("artists", "Go to Artists"),
        PalCmd::Nav("tracks", "Go to Tracks"),
        PalCmd::Nav("playlists", "Go to Playlists"),
        PalCmd::Nav("genres", "Go to Genres & Moods"),
        PalCmd::Nav("history", "Go to History"),
        PalCmd::Nav("offline", "Go to Offline"),
        PalCmd::Nav("stations", "Go to Stations"),
        PalCmd::SonicAdventure,
        PalCmd::NowPlaying,
        PalCmd::Queue,
        PalCmd::Shuffle,
        PalCmd::Repeat,
        PalCmd::Settings,
        PalCmd::ThemeCrate,
        PalCmd::ThemeOmarchy,
    ];

    fn label(self) -> &'static str {
        match self {
            PalCmd::Nav(_, l) => l,
            PalCmd::NowPlaying => "Open Now Playing (lyrics)",
            PalCmd::Queue => "Show the queue",
            PalCmd::Shuffle => "Toggle shuffle",
            PalCmd::Repeat => "Cycle repeat",
            PalCmd::Settings => "Open settings",
            PalCmd::SonicAdventure => "Sonic Adventure",
            PalCmd::ThemeCrate => "Theme: Crate",
            PalCmd::ThemeOmarchy => "Theme: Follow Omarchy",
        }
    }
}

/// A leading `@`, `#` or `>` picks a scope (canvas); returns it and the rest.
/// A leading backslash searches the text as typed (`\#1 Record`).
pub(crate) fn split_prefix(q: &str) -> (Option<char>, &str) {
    match q.chars().next() {
        Some(c @ ('@' | '#' | '>')) => (Some(c), q[1..].trim_start()),
        Some('\\') => (None, q[1..].trim_start()),
        _ => (None, q),
    }
}

impl Crate {
    pub(crate) fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette = true;
        self.pal_sel = 0;
        self.focus_search(window, cx);
        cx.notify();
    }

    pub(crate) fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette = false;
        self.reset_search = true;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// The selectable rows, in display order: top result first.
    pub(crate) fn palette_items(&self) -> Vec<(&'static str, Item)> {
        let (prefix, rest) = split_prefix(&self.search_query);
        let rest = rest.to_lowercase();
        match prefix {
            Some('#') => {
                let mut out = Vec::new();
                if let Some(h) = &self.home {
                    for (kind, group, items) in [("Genre", "Genres", &h.genres), ("Mood", "Moods", &h.moods), ("Decade", "Decades", &h.decades)] {
                        for (q, l) in items.iter().filter(|(_, l)| l.to_lowercase().contains(&rest)).take(12) {
                            out.push((group, Item::Filter { kind, label: l.clone(), query: q.clone() }));
                        }
                    }
                }
                return out;
            }
            Some('>') => {
                return PalCmd::ALL.into_iter().filter(|c| c.label().to_lowercase().contains(&rest)).map(|c| ("Commands", Item::Command(c))).collect();
            }
            _ => {}
        }
        let want = match if prefix == Some('@') { 1 } else { self.pal_scope } {
            1 => Some("artist"),
            2 => Some("album"),
            3 => Some("track"),
            _ => None,
        };
        let mut out: Vec<(&'static str, Item)> = Vec::new();
        if self.pal_scope != 4 || prefix == Some('@') {
            let hits: Vec<&SearchHit> = self.hits.iter().filter(|h| want.is_none_or(|w| h.kind == w)).collect();
            if let Some(top) = hits.first() {
                out.push(("Top result", Item::Hit((*top).clone())));
            }
            let per = if want.is_some() { 12 } else { 3 };
            for (kind, label) in [("track", "Tracks"), ("album", "Albums"), ("artist", "Artists")] {
                for h in hits.iter().skip(1).filter(|h| h.kind == kind).take(per) {
                    out.push((label, Item::Hit((*h).clone())));
                }
            }
        }
        if prefix.is_none() && (self.pal_scope == 0 || self.pal_scope == 4) {
            let q = rest.clone();
            if !q.is_empty()
                && let Some(h) = &self.home
            {
                for p in h.playlists.iter().filter(|p| p.title.to_lowercase().contains(&q)).take(4) {
                    out.push(("Playlists", Item::Playlist(p.clone())));
                }
            }
        }
        // Actions for the top artist or album.
        if self.pal_scope == 0
            && let Some((_, Item::Hit(top))) = out.first()
        {
            match top.kind.as_str() {
                "artist" => out.push(("Actions", Item::Radio(top.rating_key.clone(), format!("Start artist radio: {}", top.title)))),
                "album" => out.push(("Actions", Item::Radio(top.rating_key.clone(), format!("Start album radio: {}", top.title)))),
                _ => {}
            }
        }
        out
    }

    fn activate_item(&mut self, item: Item, window: &mut Window, cx: &mut Context<Self>) {
        match item {
            Item::Hit(h) if h.kind == "album" => {
                if let Some(ix) = self.albums.iter().position(|a| a.rating_key == h.rating_key) {
                    self.play_album(ix, cx);
                }
                self.close_palette(window, cx);
            }
            Item::Hit(h) => {
                self.activate_hit(h, cx);
                self.close_palette(window, cx);
            }
            Item::Playlist(p) => {
                self.play_playlist(p.rating_key, p.title, cx);
                self.close_palette(window, cx);
            }
            Item::Radio(key, label) => {
                let title = label.split(": ").nth(1).map(|n| format!("{n} radio")).unwrap_or(label);
                self.play_radio(&key, title, cx);
                self.close_palette(window, cx);
            }
            Item::Filter { kind, label, query } => {
                self.close_palette(window, cx);
                self.now_playing = false;
                self.add_filter(crate::library::LibFilter { kind, label, query }, cx);
            }
            Item::Command(c) => {
                self.close_palette(window, cx);
                self.run_command(c, window, cx);
            }
        }
    }

    fn run_command(&mut self, c: PalCmd, window: &mut Window, cx: &mut Context<Self>) {
        // Commands that change the page close Now Playing so the change shows.
        if matches!(c, PalCmd::Nav(..) | PalCmd::Settings | PalCmd::SonicAdventure) {
            self.now_playing = false;
        }
        match c {
            PalCmd::Nav(id, _) => self.nav_to(id, window, cx),
            PalCmd::NowPlaying => self.open_now_playing(None, cx),
            PalCmd::Queue => self.open_now_playing(Some(0), cx),
            PalCmd::Shuffle => {
                self.toggle_shuffle(cx);
                self.say(if self.shuffle { "Shuffle on" } else { "Shuffle off" });
            }
            PalCmd::Repeat => {
                self.cycle_repeat(cx);
                self.say(match self.repeat {
                    crate::queue::Repeat::Off => "Repeat off",
                    crate::queue::Repeat::All => "Repeat all",
                    crate::queue::Repeat::One => "Repeat one",
                });
            }
            PalCmd::Settings => self.open_audio_settings(cx),
            PalCmd::SonicAdventure => self.open_sonic(cx),
            PalCmd::ThemeCrate => self.set_theme_mode(crate::theme::Mode::Crate, cx),
            PalCmd::ThemeOmarchy => self.set_theme_mode(crate::theme::Mode::Omarchy, cx),
        }
        if self.now.is_none() && matches!(c, PalCmd::NowPlaying | PalCmd::Queue) {
            self.say("Nothing is playing");
        }
        cx.notify();
    }

    /// Queue an album, track or playlist (play next or at the end).
    pub(crate) fn queue_item(&mut self, item: Item, next: bool, cx: &mut Context<Self>) {
        match item {
            Item::Hit(h) if h.kind == "album" || h.kind == "track" => {
                let album_key = if h.kind == "album" { h.rating_key.clone() } else { h.parent_key.clone() };
                let Some(album) = self.albums.iter().find(|a| a.rating_key == album_key).cloned() else { return };
                self.request_art(&album);
                let (track, label) = (if h.kind == "track" { Some(h.rating_key.clone()) } else { None }, h.title.clone());
                self.with_tracks(album, cx, move |this, _album, tracks, cx| {
                    let tracks = match &track {
                        Some(k) => tracks.into_iter().filter(|t| t.rating_key == *k).collect(),
                        None => tracks,
                    };
                    this.enqueue(tracks, next, label, cx);
                });
            }
            Item::Playlist(p) => {
                let (key, label) = (p.rating_key.clone(), p.title.clone());
                let rx = crate::plex_call(move |c| async move { c.playlist_tracks(&key, 500).await });
                cx.spawn(async move |this, cx| {
                    let res = rx.await;
                    this.update(cx, |this, cx| match res {
                        Ok(Ok(tracks)) => this.enqueue(tracks, next, label, cx),
                        Ok(Err(e)) => {
                            this.status = format!("{label}: {e:#}").into();
                            cx.notify();
                        }
                        Err(_) => {}
                    })
                    .ok();
                })
                .detach();
            }
            // Artists and radio aren't a fixed list of tracks to queue.
            _ => {
                self.status = "Play next / add to queue works on albums, tracks and playlists".into();
                cx.notify();
            }
        }
    }

    /// Tab: radio from the selected artist/album (or the top result).
    pub(crate) fn palette_radio(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let items = self.palette_items();
        let pick = items.get(self.pal_sel).or(items.first()).map(|(_, i)| i.clone());
        if let Some(Item::Hit(h)) = pick
            && (h.kind == "artist" || h.kind == "album")
        {
            self.activate_item(Item::Radio(h.rating_key.clone(), format!("Radio: {}", h.title)), window, cx);
        }
    }

    fn palette_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let items = self.palette_items();
        let n = items.len();
        match ev.keystroke.key.as_str() {
            "escape" => self.close_palette(window, cx),
            "down" if n > 0 => self.pal_sel = (self.pal_sel + 1) % n,
            "up" if n > 0 => self.pal_sel = (self.pal_sel + n - 1) % n,
            "enter" if ev.keystroke.modifiers.shift || ev.keystroke.modifiers.control => {
                // Shift+Enter play next, Ctrl+Enter add to queue (canvas).
                if let Some((_, it)) = items.into_iter().nth(self.pal_sel) {
                    self.queue_item(it, ev.keystroke.modifiers.shift, cx);
                    self.close_palette(window, cx);
                }
            }
            "enter" => {
                if let Some((_, it)) = items.into_iter().nth(self.pal_sel) {
                    self.activate_item(it, window, cx);
                }
            }
            "right" => {
                // Open the selected artist/album page.
                if let Some((_, Item::Hit(h))) = items.get(self.pal_sel).cloned()
                    && h.kind != "track"
                {
                    self.activate_hit(h, cx);
                    self.close_palette(window, cx);
                } else {
                    return; // let the input move its caret
                }
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    pub(crate) fn palette_view(&self, t: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let items = self.palette_items();
        let sel = self.pal_sel.min(items.len().saturating_sub(1));
        let scopes = div().flex().gap(px(6.0)).children(SCOPES.iter().enumerate().map(|(i, s)| {
            // A typed `@` shows as the Artists scope.
            let on = match split_prefix(&self.search_query).0 {
                Some('@') => i == 1,
                Some(_) => false,
                None => i == self.pal_scope,
            };
            div()
                .id(("scope", i))
                .h(px(26.0))
                .px(px(10.0))
                .flex()
                .items_center()
                .rounded(px(13.0))
                .text_size(px(12.0))
                .font_weight(FontWeight::MEDIUM)
                .bg(rgb(if on { t.accent } else { t.raised }))
                .text_color(rgb(if on { t.base } else { t.text2 }))
                .cursor_pointer()
                .child(*s)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.pal_scope = i;
                    this.pal_sel = 0;
                    cx.notify();
                }))
        }));
        let head = div()
            .h(px(60.0))
            .px(px(18.0))
            .flex()
            .items_center()
            .gap(px(12.0))
            .border_b_1()
            .border_color(rgb(t.raised))
            .child(icons::icon("search", 20.0, t.accent))
            .child(div().flex_1().text_size(px(20.0)).font_weight(FontWeight::MEDIUM).child(Input::new(&self.search).appearance(false)))
            .child(scopes);

        let mut body = div().id("pal-body").flex_1().min_h_0().overflow_y_scroll().flex().flex_col().px(px(8.0)).pt(px(10.0)).pb(px(6.0));
        let mut last = "";
        if items.is_empty() {
            let msg = match split_prefix(&self.search_query) {
                (_, "") => "Type to search artists, albums, tracks and playlists (@ artists, # genres, > commands)",
                (Some('#'), _) if self.home.is_none() => "Loading genres…",
                (Some('#'), _) => "No genre, mood or decade matches",
                (Some('>'), _) => "No command matches",
                _ if self.search_done == self.search_gen => "No results",
                _ => "Searching…",
            };
            body = body.child(div().px(px(12.0)).py(px(16.0)).text_size(px(14.0)).text_color(rgb(t.muted)).child(msg));
        }
        for (i, (section, item)) in items.into_iter().enumerate() {
            if section != last {
                last = section;
                body = body.child(
                    div()
                        .px(px(12.0))
                        .pt(px(if i == 0 { 6.0 } else { 14.0 }))
                        .pb(px(6.0))
                        .font_family(MONO)
                        .font_weight(FontWeight::MEDIUM)
                        .text_size(px(11.0))
                        .text_color(rgb(t.muted))
                        .child(section.to_uppercase()),
                );
            }
            let on = i == sel;
            let top = section == "Top result";
            let (thumb, round, title, sub, right): (String, f32, String, String, String) = match &item {
                Item::Hit(h) => {
                    let kind = match h.kind.as_str() {
                        "artist" => "Artist",
                        "album" => "Album",
                        _ => "Track",
                    };
                    let sub = if top && !h.subtitle.is_empty() && h.subtitle != kind {
                        format!("{kind} · {}", h.subtitle)
                    } else if top {
                        kind.into()
                    } else {
                        h.subtitle.clone()
                    };
                    let right = if top {
                        match h.kind.as_str() {
                            "track" => "Enter play".into(),
                            "artist" => "Enter open · Tab radio".into(),
                            _ => "Enter play · Tab radio · → open".into(),
                        }
                    } else {
                        String::new()
                    };
                    (h.thumb.clone(), if h.kind == "artist" { 999.0 } else { 4.0 }, h.title.clone(), sub, right)
                }
                Item::Playlist(p) => (p.thumb.clone(), 4.0, p.title.clone(), format!("Playlist · {} tracks", p.count), String::new()),
                Item::Radio(_, label) => (String::new(), 8.0, label.clone(), String::new(), "Tab".into()),
                Item::Filter { kind, label, .. } => (String::new(), 8.0, label.clone(), format!("{kind} · filter your albums"), String::new()),
                Item::Command(c) => (String::new(), 8.0, c.label().into(), String::new(), "Enter".into()),
            };
            let size = if top { 52.0 } else { 36.0 };
            let icon_of = |item: &Item| match item {
                Item::Radio(..) => Some(("stations", t.accent)),
                Item::Filter { .. } => Some(("genres", t.discovery)),
                Item::Command(_) => Some(("settings", t.text2)),
                _ => None,
            };
            let lead = match icon_of(&item) {
                Some((ic, color)) => div()
                    .size(px(36.0))
                    .flex_none()
                    .rounded(px(8.0))
                    .bg(rgb(t.surface))
                    .border_1()
                    .border_color(rgb(t.border))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icons::icon(ic, 17.0, color))
                    .into_any_element(),
                None => match self.art.get(&thumb).cloned() {
                    Some(im) => img(im).size(px(size)).flex_none().rounded(px(round.min(size / 2.0))).object_fit(ObjectFit::Cover).into_any_element(),
                    None => gpui::img(self.mark(crate::art::placeholder_for(&thumb))).size(px(size)).flex_none().rounded(px(round.min(size / 2.0))).object_fit(gpui::ObjectFit::Cover).into_any_element(),
                },
            };
            body = body.child(
                div()
                    .id(("pal", i))
                    .px(px(12.0))
                    .py(px(if top { 10.0 } else { 8.0 }))
                    .flex()
                    .items_center()
                    .gap(px(14.0))
                    .rounded(px(if top { 10.0 } else { 8.0 }))
                    .cursor_pointer()
                    .when(on, |d| d.bg(rgb(t.raised)).border_l_2().border_color(rgb(t.accent)))
                    .hover(|d| d.bg(rgb(t.raised)))
                    .child(lead)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(3.0))
                            .child(
                                div()
                                    .text_size(px(if top { 16.0 } else { 14.0 }))
                                    .font_weight(if top { FontWeight::SEMIBOLD } else { FontWeight::NORMAL })
                                    .text_color(rgb(t.text_strong))
                                    .truncate()
                                    .child(title),
                            )
                            .when(!sub.is_empty(), |d| d.child(div().text_size(px(12.0)).text_color(rgb(if top { t.text2 } else { t.muted })).truncate().child(sub))),
                    )
                    .when(!right.is_empty(), |d| {
                        d.child(div().font_family(MONO).font_weight(FontWeight::MEDIUM).text_size(px(11.0)).text_color(rgb(t.text2)).child(right))
                    })
                    .on_click(cx.listener(move |this, _, window, cx| this.activate_item(item.clone(), window, cx))),
            );
        }
        let foot = div()
            .px(px(18.0))
            .py(px(12.0))
            .flex()
            .gap(px(18.0))
            .border_t_1()
            .border_color(rgb(t.raised))
            .bg(rgb(t.base))
            .font_family(MONO)
            .text_size(px(11.0))
            .text_color(rgb(t.muted))
            .child("↑↓ move")
            .child("Enter play")
            .child("⇧Enter next")
            .child("Ctrl+Enter queue")
            .child("Tab radio")
            .child("→ open")
            .child("@ artists  # genres  > commands")
            .child(div().flex_1())
            .child("Esc");

        div()
            .id("palette-backdrop")
            .absolute()
            .inset_0()
            .bg(gpui::hsla(240.0 / 360.0, 0.2, 0.05, 0.6))
            .flex()
            .justify_center()
            .on_click(cx.listener(|this, _, window, cx| this.close_palette(window, cx)))
            .child(
                div()
                    .id("palette")
                    .mt(px(96.0))
                    .w(px(760.0))
                    .max_h(px(680.0))
                    .rounded(px(16.0))
                    .bg(rgb(t.surface))
                    .border_1()
                    .border_color(rgb(t.line))
                    .shadow_2xl()
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| this.palette_key(ev, window, cx)))
                    .key_context("Palette")
                    .on_action(cx.listener(|this, _: &crate::PaletteRadio, window, cx| this.palette_radio(window, cx)))
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(head)
                    .child(body)
                    .child(foot),
            )
            .into_any_element()
    }
}
