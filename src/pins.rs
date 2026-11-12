//! Pinned items in the sidebar: playlists, albums, artists, mixes and
//! stations you choose, in your order. Saved per machine in
//! ~/.config/crate/pins.json. Pin or unpin from each item's page (the Pin
//! button), the sidebar's + (your playlists), or a pin's right-click menu;
//! drag pins to reorder them.

use gpui::{Context, FontWeight, ObjectFit, div, img, prelude::*, px, rgb};
use gpui_component::menu::{ContextMenuExt as _, PopupMenuItem};
use serde::{Deserialize, Serialize};

use crate::{Crate, icons, mix::MixSource, shell::MONO, theme::Theme};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum PinKind {
    Playlist,
    Album,
    Artist,
    Mix,
    Station,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Pin {
    pub kind: PinKind,
    /// Rating key (playlist, album, artist), station key, or mix id.
    pub key: String,
    pub title: String,
    #[serde(default)]
    pub thumb: String,
    /// A mix's source and description (to open its page again).
    #[serde(default)]
    pub mix: Option<MixSource>,
    #[serde(default)]
    pub desc: String,
}

/// A pin being dragged to a new place in the sidebar.
#[derive(Clone)]
pub(crate) struct PinDrag {
    from: usize,
}

pub(crate) struct PinGhost {
    title: String,
    bg: u32,
    text: u32,
}

impl gpui::Render for PinGhost {
    fn render(&mut self, _: &mut gpui::Window, _: &mut Context<Self>) -> impl IntoElement {
        div().px(px(10.0)).py(px(6.0)).rounded(px(8.0)).bg(rgb(self.bg)).text_size(px(13.0)).text_color(rgb(self.text)).child(self.title.clone())
    }
}

fn path() -> std::path::PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config"));
    base.join("crate/pins.json")
}

pub(crate) fn load() -> Vec<Pin> {
    std::fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

impl Crate {
    fn save_pins(&mut self) {
        let p = path();
        let res = (|| -> std::io::Result<()> {
            std::fs::create_dir_all(p.parent().unwrap_or(std::path::Path::new(".")))?;
            let tmp = p.with_extension(format!("tmp{}", std::process::id()));
            std::fs::write(&tmp, serde_json::to_vec_pretty(&self.pins)?)?;
            std::fs::rename(tmp, &p)
        })();
        if let Err(e) = res {
            self.say(format!("Couldn't save your pins: {e}"));
        }
    }

    pub(crate) fn is_pinned(&self, kind: PinKind, key: &str) -> bool {
        self.pins.iter().any(|p| p.kind == kind && p.key == key)
    }

    /// Pin it (at the end), or unpin it if it's pinned.
    pub(crate) fn toggle_pin(&mut self, pin: Pin, cx: &mut Context<Self>) {
        if let Some(i) = self.pins.iter().position(|p| p.kind == pin.kind && p.key == pin.key) {
            let gone = self.pins.remove(i);
            self.say(format!("Unpinned {}", gone.title));
        } else {
            self.request_thumb(&pin.thumb);
            self.say(format!("Pinned {} to the sidebar", pin.title));
            self.pins.push(pin);
        }
        self.save_pins();
        cx.notify();
    }

    fn move_pin(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        if from == to || from >= self.pins.len() || to >= self.pins.len() {
            return;
        }
        let p = self.pins.remove(from);
        self.pins.insert(to, p);
        self.save_pins();
        cx.notify();
    }

    fn open_pin(&mut self, pin: Pin, cx: &mut Context<Self>) {
        self.now_playing = false;
        match pin.kind {
            PinKind::Playlist => self.open_playlist(pin.key, pin.title, cx),
            PinKind::Album => match self.albums.iter().position(|a| a.rating_key == pin.key) {
                Some(ix) => self.open_album(ix, cx),
                None => self.say(format!("{} isn't in the library any more", pin.title)),
            },
            PinKind::Artist => self.open_artist(pin.key, pin.title, cx),
            PinKind::Mix => match pin.mix {
                Some(source) => self.open_mix(pin.key, pin.title, pin.desc, source, cx),
                None => self.say("That pin is missing its mix details; pin it again"),
            },
            // Stations have no page: a pin starts it.
            PinKind::Station => self.play_station(pin.key, pin.title, cx),
        }
        cx.notify();
    }

    /// A "Pin" / "Pinned" pill for an item's page.
    pub(crate) fn pin_pill(&self, id: &'static str, pin: Pin, t: &Theme, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let on = self.is_pinned(pin.kind, &pin.key);
        div()
            .id(id)
            .h(px(34.0))
            .px(px(14.0))
            .flex()
            .items_center()
            .gap(px(6.0))
            .rounded(px(17.0))
            .border_1()
            .border_color(rgb(if on { t.accent } else { t.line }))
            .text_size(px(13.0))
            .text_color(rgb(if on { t.accent } else { t.text }))
            .cursor_pointer()
            .hover(|d| d.bg(rgb(t.raised)))
            .child(if on { "Pinned" } else { "Pin to sidebar" })
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_pin(pin.clone(), cx)))
    }

    /// The sidebar's pins (or a hint when there are none).
    pub(crate) fn sidebar_pins(&self, t: &Theme, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        if self.pins.is_empty() {
            return vec![
                div()
                    .px(px(10.0))
                    .py(px(6.0))
                    .text_size(px(12.0))
                    .text_color(rgb(t.muted))
                    .child("Pin playlists, albums, artists and mixes from their pages, or with +.")
                    .into_any_element(),
            ];
        }
        let swatch = [t.yellow, t.green, t.cyan, t.magenta, t.red, t.orange];
        let entity = cx.entity().downgrade();
        self.pins
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let icon = match p.kind {
                    PinKind::Playlist => "playlists",
                    PinKind::Album => "albums",
                    PinKind::Artist => "artists",
                    PinKind::Mix => "shuffle",
                    PinKind::Station => "stations",
                };
                let round = if p.kind == PinKind::Artist { 14.0 } else { 5.0 };
                let tile = match self.art.get(&p.thumb).cloned() {
                    Some(im) => img(im).size(px(28.0)).flex_none().rounded(px(round)).object_fit(ObjectFit::Cover).into_any_element(),
                    None => div()
                        .size(px(28.0))
                        .flex_none()
                        .rounded(px(round))
                        .bg(rgb(swatch[i % swatch.len()]))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(icons::icon(icon, 13.0, t.base))
                        .into_any_element(),
                };
                let kind = match p.kind {
                    PinKind::Playlist => "",
                    PinKind::Album => "ALBUM",
                    PinKind::Artist => "ARTIST",
                    PinKind::Mix => "MIX",
                    PinKind::Station => "RADIO",
                };
                let (open, unpin) = (p.clone(), p.clone());
                let (accent, bg, text, title) = (t.accent, t.raised, t.text, p.title.clone());
                let e = entity.clone();
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
                    .child(div().font_family(MONO).font_weight(FontWeight::MEDIUM).text_size(px(9.0)).text_color(rgb(t.muted)).child(kind))
                    .on_drag(PinDrag { from: i }, move |_, _, _, cx| {
                        let title = title.clone();
                        cx.new(|_| PinGhost { title, bg, text })
                    })
                    .drag_over::<PinDrag>(move |st, dr, _, _| if dr.from < i { st.border_b_2().border_color(rgb(accent)) } else if dr.from > i { st.border_t_2().border_color(rgb(accent)) } else { st })
                    .on_drop(cx.listener(move |this, dr: &PinDrag, _, cx| this.move_pin(dr.from, i, cx)))
                    .on_click(cx.listener(move |this, _, _, cx| this.open_pin(open.clone(), cx)))
                    .context_menu(move |menu, _, _| {
                        let (e, pin) = (e.clone(), unpin.clone());
                        menu.item(PopupMenuItem::new("Unpin").on_click(move |_, _, cx| {
                            e.update(cx, |this, cx| this.toggle_pin(pin.clone(), cx)).ok();
                        }))
                    })
                    .into_any_element()
            })
            .collect()
    }

    /// The sidebar's +: your playlists, to pin or unpin.
    pub(crate) fn pin_add_button(&self, t: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        use gpui_component::button::{Button, ButtonVariants as _};
        use gpui_component::menu::DropdownMenu as _;
        use gpui_component::Sizable as _;
        let lists = self.home.as_ref().map(|h| h.playlists.clone()).unwrap_or_default();
        let pinned: Vec<String> = self.pins.iter().filter(|p| p.kind == PinKind::Playlist).map(|p| p.key.clone()).collect();
        let entity = cx.entity().downgrade();
        let _ = t;
        Button::new("pin-add")
            .ghost()
            .xsmall()
            .label("+")
            .dropdown_menu(move |mut menu, _, _| {
                if lists.is_empty() {
                    return menu.item(PopupMenuItem::new("No playlists yet").disabled(true));
                }
                for p in lists.iter().take(40) {
                    let e = entity.clone();
                    let pin = Pin { kind: PinKind::Playlist, key: p.rating_key.clone(), title: p.title.clone(), thumb: p.thumb.clone(), mix: None, desc: String::new() };
                    let on = pinned.contains(&p.rating_key);
                    menu = menu.item(PopupMenuItem::new(p.title.clone()).checked(on).on_click(move |_, _, cx| {
                        e.update(cx, |this, cx| this.toggle_pin(pin.clone(), cx)).ok();
                    }));
                }
                menu
            })
            .into_any_element()
    }
}
