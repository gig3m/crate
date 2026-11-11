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
    /// A smart playlist from the library's album filter query.
    Smart(String),
}

pub(crate) struct Naming {
    pub input: Entity<InputState>,
    pub what: NameFor,
}

impl Crate {
    /// Open the naming bar with `default` selected, ready to type over.
    pub(crate) fn start_naming(&mut self, what: NameFor, default: String, window: &mut Window, cx: &mut Context<Self>) {
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
        cx.notify();
    }

    /// Create the named playlist on the server.
    pub(crate) fn commit_naming(&mut self, cx: &mut Context<Self>) {
        let Some(n) = self.naming.as_ref() else { return };
        let title = n.input.read(cx).value().trim().to_string();
        if title.is_empty() {
            return;
        }
        let Some(n) = self.naming.take() else { return };
        let section = crate::SECTION.get().cloned().unwrap_or_default();
        let (rx, smart) = match n.what {
            NameFor::Albums(keys) => {
                self.selection.clear();
                let t = title.clone();
                (crate::plex_call(move |c| async move { c.create_playlist(&t, &keys).await }), false)
            }
            NameFor::Smart(query) => {
                let t = title.clone();
                (crate::plex_call(move |c| async move { c.create_smart_playlist(&section, &t, &query).await }), true)
            }
        };
        self.say(format!("Creating \u{201c}{title}\u{201d}…"));
        cx.spawn(async move |this, cx| {
            let res = rx.await;
            this.update(cx, |this, cx| {
                match res {
                    Ok(Ok((_, n))) => {
                        let kind = if smart { "Smart playlist" } else { "Playlist" };
                        this.say(format!("{kind} \u{201c}{title}\u{201d} created · {n} tracks"));
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
        let keys = std::mem::take(&mut self.selection);
        if keys.is_empty() {
            return;
        }
        let rx = crate::plex_call(move |c| async move { c.add_to_playlist(&playlist, &keys).await });
        cx.spawn(async move |this, cx| {
            let res = rx.await;
            this.update(cx, |this, cx| {
                match res {
                    Ok(Ok(0)) => this.say(format!("Already in \u{201c}{title}\u{201d}: Plex skips tracks a playlist has")),
                    Ok(Ok(n)) => {
                        this.say(format!("Added {n} tracks to \u{201c}{title}\u{201d}"));
                        this.reload_playlists(cx);
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

    /// Refresh the playlist list (sidebar, Playlists page, menus).
    fn reload_playlists(&mut self, cx: &mut Context<Self>) {
        let rx = crate::plex_call(|c| async move { c.playlists().await });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(lists)) = rx.await else { return };
            this.update(cx, |this, cx| {
                for p in &lists {
                    this.request_thumb(&p.thumb);
                }
                if let Some(h) = this.home.as_mut() {
                    h.playlists = lists;
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
            NameFor::Albums(ref k) => format!("New playlist · {} album{}", k.len(), if k.len() == 1 { "" } else { "s" }),
            NameFor::Smart(_) => "New smart playlist · updates as the library changes".to_string(),
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
                                .child(btn("naming-save", "Create", true).on_click(cx.listener(|this, _, _, cx| this.commit_naming(cx))))
                                .child(btn("naming-cancel", "Cancel", false).on_click(cx.listener(|this, _, _, cx| this.cancel_naming(cx)))),
                        ),
                )
                .into_any_element(),
        )
    }
}
