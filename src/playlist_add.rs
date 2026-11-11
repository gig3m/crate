//! Writing playlists to Plex (canvas Library page): "Add to playlist" on a
//! multi-selection (a new playlist, or append to one of yours), and "Save as
//! smart playlist" for the library's current filters. A new playlist is
//! named in a small bar at the bottom of the library.

use gpui::{Context, Entity, FontWeight, Window, div, prelude::*, px, rgb};
use gpui_component::input::{Input, InputEvent, InputState};

use crate::{Crate, theme::Theme};

pub(crate) enum NameFor {
    /// A playlist of these albums' tracks.
    Albums(Vec<String>),
    /// A playlist of these tracks, in this order (a mix or the queue).
    Tracks(Vec<String>),
    /// A smart playlist from the library's album filter query.
    Smart(String),
    /// A new name for this playlist.
    Rename(String),
}

pub(crate) struct Naming {
    pub input: Entity<InputState>,
    pub what: NameFor,
}

impl Crate {
    /// Open the naming bar with `default` selected, ready to type over.
    pub(crate) fn start_naming(&mut self, what: NameFor, default: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.playlist_busy {
            self.say("Still saving the last playlist…");
            return;
        }
        // The bar lives on the page: get Now Playing / the tile out of the way.
        self.now_playing = false;
        self.compact_playing = false;
        let input = cx.new(|cx| InputState::new(window, cx).default_value(default));
        cx.subscribe(&input, |this, _, ev: &InputEvent, cx| {
            if let InputEvent::PressEnter { .. } = ev {
                this.commit_naming(cx);
            }
        })
        .detach();
        input.update(cx, |s, cx| {
            s.focus(window, cx);
            s.select_all(window, cx);
        });
        self.naming = Some(Naming { input, what });
        cx.notify();
    }

    pub(crate) fn cancel_naming(&mut self, cx: &mut Context<Self>) {
        self.naming = None;
        self.refocus = true;
        cx.notify();
    }

    /// Create the named playlist on the server.
    pub(crate) fn commit_naming(&mut self, cx: &mut Context<Self>) {
        let Some(n) = self.naming.as_ref() else { return };
        let title = n.input.read(cx).value().trim().to_string();
        if title.is_empty() {
            return;
        }
        if self.playlist_busy {
            return;
        }
        let Some(n) = self.naming.take() else { return };
        self.refocus = true;
        self.playlist_busy = true;
        let section = crate::section();
        let smart = matches!(n.what, NameFor::Smart(_));
        let chunked = matches!(n.what, NameFor::Tracks(_));
        let (rx, albums) = match n.what {
            NameFor::Albums(keys) => {
                let (t, k) = (title.clone(), keys.clone());
                (crate::plex_call(move |c| async move { c.create_playlist(&t, &k).await }), Some(keys))
            }
            NameFor::Tracks(keys) => {
                let t = title.clone();
                (crate::plex_call(move |c| async move { c.create_playlist_chunked(&t, &keys).await }), None)
            }
            NameFor::Smart(query) => {
                let t = title.clone();
                (crate::plex_call(move |c| async move { c.create_smart_playlist(&section, &t, &query).await }), None)
            }
            NameFor::Rename(key) => {
                self.playlist_busy = false;
                self.rename_playlist(key, title, cx);
                return;
            }
        };
        self.say(format!("Creating \u{201c}{title}\u{201d}…"));
        cx.spawn(async move |this, cx| {
            let res = rx.await;
            this.update(cx, |this, cx| {
                this.playlist_busy = false;
                match res {
                    Ok(Ok((_, n))) => {
                        // Done with the selection (kept on failure, to retry),
                        // unless it has changed since.
                        if albums.as_ref().is_some_and(|k| *k == this.selection) {
                            this.selection.clear();
                        }
                        let kind = if smart { "Smart playlist" } else { "Playlist" };
                        this.say(format!("{kind} \u{201c}{title}\u{201d} created · {n} track{}", if n == 1 { "" } else { "s" }));
                        this.reload_playlists(cx);
                    }
                    // A long save can fail part-way with the playlist made:
                    // show it in the sidebar either way.
                    Ok(Err(e)) if chunked => {
                        this.say(format!("\u{201c}{title}\u{201d}: {e:#}"));
                        this.reload_playlists(cx);
                    }
                    Ok(Err(e)) => this.say(format!("Couldn't create \u{201c}{title}\u{201d}: {e:#}")),
                    Err(_) => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Append the selected albums to an existing playlist.
    pub(crate) fn add_selection_to_playlist(&mut self, playlist: String, title: String, cx: &mut Context<Self>) {
        if self.playlist_busy {
            self.say("Still saving the last playlist…");
            return;
        }
        let keys = std::mem::take(&mut self.selection);
        self.add_items_to_playlist(keys, playlist, title, cx);
    }

    /// Append albums or tracks (rating keys) to an existing playlist.
    pub(crate) fn add_items_to_playlist(&mut self, keys: Vec<String>, playlist: String, title: String, cx: &mut Context<Self>) {
        if self.playlist_busy {
            self.say("Still saving the last playlist…");
            return;
        }
        if keys.is_empty() {
            return;
        }
        self.playlist_busy = true;
        let key = playlist.clone();
        let rx = crate::plex_call(move |c| async move { c.add_to_playlist(&playlist, &keys).await });
        cx.spawn(async move |this, cx| {
            let res = rx.await;
            this.update(cx, |this, cx| {
                this.playlist_busy = false;
                match res {
                    Ok(Ok(0)) => this.say(format!("Already in \u{201c}{title}\u{201d}: Plex skips tracks a playlist has")),
                    Ok(Ok(n)) => {
                        this.say(format!("Added {n} track{} to \u{201c}{title}\u{201d}", if n == 1 { "" } else { "s" }));
                        this.reload_playlists(cx);
                        // Its page, open or in history, shows the new tracks.
                        this.refresh_playlist_page(&key, cx);
                    }
                    Ok(Err(e)) => this.say(format!("Couldn't add to \u{201c}{title}\u{201d}: {e:#}")),
                    Err(_) => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// An "+ Playlist" pill: a new playlist of `keys` (named `name` by
    /// default), or add them to one of your playlists.
    pub(crate) fn playlist_menu(&self, id: impl Into<gpui::ElementId>, keys: Vec<String>, name: String, cx: &mut Context<Self>) -> gpui::AnyElement {
        self.playlist_menu_with(id, keys, name, None, cx)
    }

    /// The same menu as a small "+" icon, for track rows: its clicks stay
    /// out of the row (which plays the track).
    pub(crate) fn playlist_menu_icon(&self, id: impl Into<gpui::ElementId>, keys: Vec<String>, name: String, color: u32, cx: &mut Context<Self>) -> gpui::AnyElement {
        let id = id.into();
        div()
            .id(gpui::ElementId::Name(format!("{id}-wrap").into()))
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(self.playlist_menu_with(id, keys, name, Some(color), cx))
            .into_any_element()
    }

    fn playlist_menu_with(&self, id: impl Into<gpui::ElementId>, keys: Vec<String>, name: String, icon: Option<u32>, cx: &mut Context<Self>) -> gpui::AnyElement {
        use gpui_component::{Sizable, button::{Button, ButtonVariants}, menu::{DropdownMenu, PopupMenuItem}};
        let this = cx.entity().downgrade();
        let lists: Vec<(String, String)> =
            self.home.as_ref().map(|h| h.playlists.iter().filter(|p| !p.smart).map(|p| (p.rating_key.clone(), p.title.clone())).collect()).unwrap_or_default();
        let button = match icon {
            Some(c) => Button::new(id).ghost().xsmall().tooltip("Add to playlist").child(crate::icons::icon("plus", 14.0, c)),
            None => Button::new(id).ghost().label("+ Playlist"),
        };
        button
            .dropdown_menu(move |mut menu, _, _| {
                let (t2, k2, n2) = (this.clone(), keys.clone(), name.clone());
                menu = menu.item(PopupMenuItem::new("New playlist…").on_click(move |_, window, cx| {
                    let (k, n) = (k2.clone(), n2.clone());
                    t2.update(cx, |this, cx| this.start_naming(NameFor::Albums(k), n, window, cx)).ok();
                }));
                if !lists.is_empty() {
                    menu = menu.separator();
                }
                for (key, title) in &lists {
                    let (key, title, this, keys) = (key.clone(), title.clone(), this.clone(), keys.clone());
                    menu = menu.item(PopupMenuItem::new(title.clone()).on_click(move |_, _, cx| {
                        this.update(cx, |this, cx| this.add_items_to_playlist(keys.clone(), key.clone(), title.clone(), cx)).ok();
                    }));
                }
                menu
            })
            .into_any_element()
    }

    /// Refresh the playlist list (sidebar, Playlists page, menus).
    pub(crate) fn reload_playlists(&mut self, cx: &mut Context<Self>) {
        let rx = crate::plex_call(|c| async move { c.playlists().await });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(lists)) = rx.await else { return };
            this.update(cx, |this, cx| {
                for p in &lists {
                    this.request_thumb(&p.thumb);
                }
                if let Some(h) = this.home.as_mut() {
                    h.playlists = lists;
                    this.playlists_gen += 1;
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Default name for a smart playlist: its filters, "Rock · 1990s".
    pub(crate) fn smart_playlist_name(&self) -> String {
        self.lib_filters.iter().map(|f| f.label.as_str()).collect::<Vec<_>>().join(" · ")
    }

    /// Default name for a playlist of the selected albums.
    pub(crate) fn selection_playlist_name(&self) -> String {
        let first = self.selection.first().and_then(|k| self.albums.iter().find(|a| &a.rating_key == k)).map(|a| a.title.clone()).unwrap_or_default();
        match self.selection.len() {
            0 | 1 => first,
            n => format!("{first} + {} more", n - 1),
        }
    }

    /// The naming bar, in place of the selection bar while it's open.
    pub(crate) fn naming_bar(&self, t: &Theme, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let n = self.naming.as_ref()?;
        let what = match n.what {
            NameFor::Albums(ref k) if k.len() == 1 => "New playlist".to_string(),
            NameFor::Albums(ref k) => format!("New playlist · {} albums", k.len()),
            NameFor::Tracks(ref k) => format!("New playlist · {} tracks", k.len()),
            NameFor::Smart(_) => "New smart playlist · updates as the library changes".to_string(),
            NameFor::Rename(_) => "Rename playlist".to_string(),
        };
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
                        .id("naming-bar")
                        .block_mouse_except_scroll()
                        .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                            if ev.keystroke.key == "escape" {
                                this.cancel_naming(cx);
                                cx.stop_propagation();
                            }
                        }))
                        .flex()
                        .flex_col()
                        .gap(px(8.0))
                        .p(px(12.0))
                        .w(px(460.0))
                        .rounded(px(14.0))
                        .bg(rgb(t.raised))
                        .border_1()
                        .border_color(rgb(t.line))
                        .shadow_2xl()
                        .child(div().text_size(px(12.0)).text_color(rgb(t.muted)).child(what))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .child(
                                    div()
                                        .flex_1()
                                        .h(px(34.0))
                                        .px(px(4.0))
                                        .rounded(px(8.0))
                                        .border_1()
                                        .border_color(rgb(t.border))
                                        .bg(rgb(t.surface))
                                        .child(Input::new(&n.input).appearance(false)),
                                )
                                .child(btn("naming-save", if matches!(n.what, NameFor::Rename(_)) { "Rename" } else { "Create" }, true).on_click(cx.listener(|this, _, _, cx| this.commit_naming(cx))))
                                .child(btn("naming-cancel", "Cancel", false).on_click(cx.listener(|this, _, _, cx| this.cancel_naming(cx)))),
                        ),
                )
                .into_any_element(),
        )
    }
}
