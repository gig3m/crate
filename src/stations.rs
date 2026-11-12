//! Stations page from the design canvas (SonicAdventure.dc.html), built from
//! what the server offers today: Plex stations, decade/mood/genre radio, and
//! Sonic Adventure's status while sonic analysis runs.

use gpui::{Context, FontWeight, div, linear_color_stop, linear_gradient, prelude::*, px, rgb};

use gpui_component::menu::ContextMenuExt as _;

use crate::{Crate, PAD_X, icons, shell::MONO, theme::{Theme, mix}};

impl Crate {
    pub(crate) fn refresh_sonic_progress(&mut self, cx: &mut Context<Self>) {
        let rx = crate::plex_call(|c| async move { c.sonic_progress().await });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(p)) = rx.await {
                this.update(cx, |this, cx| {
                    this.sonic_progress = p;
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    pub(crate) fn stations_view(&self, t: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let home = self.home.as_ref();
        let h2 = |s: &'static str| div().text_size(px(18.0)).font_weight(FontWeight::SEMIBOLD).text_color(rgb(t.text_strong)).child(s);

        // Sonic Adventure: honest status until the server can compute paths.
        let progress = self.sonic_progress;
        let sonic = div()
            .id("sonic-card")
            .cursor_pointer()
            .on_click(cx.listener(|this, _, _, cx| this.open_sonic(cx)))
            .rounded(px(16.0))
            .p(px(18.0))
            .bg(linear_gradient(120.0, linear_color_stop(rgb(mix(t.surface, t.discovery, 0.14)), 0.0), linear_color_stop(rgb(t.surface), 0.7)))
            .border_1()
            .border_color(rgb(t.raised))
            .flex()
            .items_center()
            .gap(px(18.0))
            .child(
                div()
                    .size(px(52.0))
                    .flex_none()
                    .rounded(px(12.0))
                    .bg(rgb(t.raised))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icons::icon("sonic", 24.0, t.discovery)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(div().text_size(px(16.0)).font_weight(FontWeight::SEMIBOLD).text_color(rgb(t.text_strong)).child("Sonic Adventure"))
                    .child(div().text_size(px(13.0)).text_color(rgb(t.muted)).child(match progress {
                        Some(p) => format!(
                            "A playlist that travels between two tracks, step by sonic step. Your server is still analyzing the library ({p}% done), so paths may skip unanalyzed albums."
                        ),
                        None => "A playlist that travels between two tracks, step by sonic step. Pick a start and an end.".into(),
                    }))
                    .when_some(progress, |d, p| {
                        d.child(
                            div()
                                .mt(px(4.0))
                                .w(px(320.0))
                                .h(px(4.0))
                                .rounded(px(2.0))
                                .bg(rgb(t.raised))
                                .child(div().h_full().rounded(px(2.0)).w(gpui::relative(p as f32 / 100.0)).bg(rgb(t.discovery))),
                        )
                    }),
            );

        // Plex stations.
        let stations: Vec<_> = home
            .map(|h| h.rows.iter().filter(|r| r.kind == "station").flat_map(|r| r.items.iter().cloned()).collect::<Vec<_>>())
            .unwrap_or_default()
            .into_iter()
            .enumerate()
            .map(|(i, s)| {
                let (key, title) = (s.key.clone(), s.title.clone());
                div()
                    .id(("st", i))
                    .flex_1()
                    .min_w(px(200.0))
                    .p(px(14.0))
                    .rounded(px(12.0))
                    .border_1()
                    .border_color(rgb(t.border))
                    .bg(rgb(t.surface))
                    .flex()
                    .items_center()
                    .gap(px(14.0))
                    .cursor_pointer()
                    .hover(|d| d.bg(rgb(t.raised)))
                    .child(gpui::img(self.mark(crate::art::station_motif(&s.title))).size(px(44.0)).flex_none().rounded(px(22.0)))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(4.0))
                            .child(div().text_size(px(14.0)).font_weight(FontWeight::SEMIBOLD).text_color(rgb(t.text_strong)).child(s.title.clone()))
                            .child(div().text_size(px(12.0)).text_color(rgb(t.muted)).child(crate::home::station_blurb(&s.title))),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.play_station(key.clone(), title.clone(), cx)))
                    .context_menu({
                        let (e, pin) = (cx.entity().downgrade(), crate::pins::Pin { kind: crate::pins::PinKind::Station, key: s.key.clone(), title: s.title.clone(), thumb: String::new(), mix: None, desc: String::new() });
                        let pinned = self.is_pinned(crate::pins::PinKind::Station, &s.key);
                        move |menu, _, _| {
                            let (e, pin) = (e.clone(), pin.clone());
                            menu.item(gpui_component::menu::PopupMenuItem::new(if pinned { "Unpin from sidebar" } else { "Pin to sidebar" }).on_click(move |_, _, cx| {
                                e.update(cx, |this, cx| this.toggle_pin(pin.clone(), cx)).ok();
                            }))
                        }
                    })
            })
            .collect();

        // Decade / mood / genre radio: random tracks matching the tag, refilled as it plays.
        let chip_row = |label: &'static str, items: Vec<(String, String)>, mono: bool, cx: &mut Context<Self>| {
            div()
                .flex()
                .flex_col()
                .gap(px(10.0))
                .child(div().font_family(MONO).font_weight(FontWeight::MEDIUM).text_size(px(11.0)).text_color(rgb(t.muted)).child(label))
                .child(div().flex().flex_wrap().gap(px(8.0)).children(items.into_iter().enumerate().map(|(i, (q, l))| {
                    let title = format!("{l} radio");
                    div()
                        .id((label, i))
                        .h(px(30.0))
                        .px(px(12.0))
                        .flex()
                        .items_center()
                        .rounded(px(if mono { 6.0 } else { 15.0 }))
                        .border_1()
                        .border_color(rgb(t.line))
                        .text_color(rgb(t.text))
                        .font_weight(FontWeight::MEDIUM)
                        .when(mono, |d| d.font_family(MONO).text_size(px(12.0)))
                        .when(!mono, |d| d.text_size(px(13.0)))
                        .cursor_pointer()
                        .hover(|d| d.bg(rgb(t.raised)))
                        .child(l)
                        .on_click(cx.listener(move |this, _, _, cx| this.play_station(format!("query:{q}"), title.clone(), cx)))
                })))
        };
        let decades: Vec<(String, String)> =
            home.map(|h| h.decades.iter().map(|(q, l)| (q.replace("decade=", "album.decade="), l.clone())).collect()).unwrap_or_default();
        let moods = home.map(|h| h.radio_moods.clone()).unwrap_or_default();
        let genres = home.map(|h| h.genres.clone()).unwrap_or_default();

        div()
            .id("stations")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px(px(PAD_X))
            .pt(px(4.0))
            .pb(px(40.0))
            .flex()
            .flex_col()
            .gap(px(26.0))
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(14.0))
                    .child(div().text_size(px(30.0)).font_weight(FontWeight::BOLD).text_color(rgb(t.text_strong)).child("Stations"))
                    .child(div().text_size(px(14.0)).text_color(rgb(t.muted)).child("Endless radio from your library")),
            )
            .child(sonic)
            .child(div().flex().flex_col().gap(px(12.0)).child(h2("Radio")).child(div().flex().flex_wrap().gap(px(10.0)).children(stations)))
            .child(chip_row("DECADE RADIO", decades, true, cx))
            .child(chip_row("MOOD RADIO", moods, false, cx))
            .child(chip_row("GENRE RADIO", genres, false, cx))
            .child(
                div()
                    .text_size(px(13.0))
                    .text_color(rgb(t.muted))
                    .child("Artist and album radio: the Artist radio button on any artist page, or Tab on a result in Ctrl K."),
            )
            .into_any_element()
    }
}
