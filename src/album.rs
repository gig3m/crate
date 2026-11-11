//! Album page from the design canvas (Album.dc.html).

use gpui::{Context, FontWeight, ObjectFit, div, img, linear_color_stop, linear_gradient, prelude::*, px, rgb};

use crate::{Crate, PAD_X, View, fmt_ms, icons, plex::Track, shell::MONO, theme::{Theme, mix}};

/// Clickable stars: `on_rate(n)` with n in 1..=5.
pub(crate) fn rate_stars(
    id: impl Into<gpui::SharedString>,
    rating: Option<f32>,
    size: f32,
    on: u32,
    off: u32,
    cx: &mut Context<Crate>,
    on_rate: impl Fn(&mut Crate, u8, &mut Context<Crate>) + Clone + 'static,
) -> gpui::Div {
    let id: gpui::SharedString = id.into();
    let full = rating.map(|r| (r / 2.0).floor() as usize).unwrap_or(0);
    div().flex().gap(px(2.0)).children((0..5u8).map(|i| {
        let f = on_rate.clone();
        div()
            .id(gpui::SharedString::from(format!("{id}-{i}")))
            .cursor_pointer()
            .child(if (i as usize) < full { icons::icon("star", size, on) } else { icons::icon("star-outline", size, off) })
            .on_click(cx.listener(move |this, _, _, cx| f(this, i + 1, cx)))
    }))
}

fn long_date(iso: &str) -> String {
    const M: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
    let p: Vec<&str> = iso.split('-').collect();
    match (p.first(), p.get(1).and_then(|m| m.parse::<usize>().ok()), p.get(2).and_then(|d| d.parse::<u32>().ok())) {
        (Some(y), Some(m), Some(d)) if (1..=12).contains(&m) => format!("{} {d}, {y}", M[m - 1]),
        _ => iso.to_string(),
    }
}

fn month_year(ts: i64) -> String {
    // Civil-from-days (Howard Hinnant), enough for "Jan 2024".
    let days = ts.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    const M: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    format!("{} {y}", M[(m - 1) as usize])
}

impl Crate {
    pub(crate) fn request_album_detail(&mut self, key: String, cx: &mut Context<Self>) {
        if key.is_empty() || self.album_detail.contains_key(&key) {
            return;
        }
        self.album_detail.insert(key.clone(), None);
        let k = key.clone();
        let rx = crate::plex_call(move |c| async move { c.album_detail(&k).await });
        cx.spawn(async move |this, cx| match rx.await {
            Ok(Ok(d)) => {
                this.update(cx, |this, cx| {
                    this.album_detail.insert(key, Some(d));
                    cx.notify();
                })
                .ok();
            }
            // Leave the None marker: retrying here would refetch on every 250 ms poll.
            Ok(Err(e)) => eprintln!("[crate] album detail {key}: {e:#}"),
            Err(_) => {}
        })
        .detach();
    }

    fn album_tracks(&self) -> Option<Vec<Track>> {
        match &self.view {
            View::Album { tracks: Some(tr), .. } => Some(tr.clone()),
            _ => None,
        }
    }

    pub(crate) fn album_view(&self, ix: usize, tracks: Option<&Vec<Track>>, t: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(album) = self.album(ix).cloned() else { return div().into_any_element() };
        let detail = self.album_detail.get(&album.rating_key).cloned().flatten();
        let playing_here = self.now.as_ref().filter(|n| n.context_key.as_deref() == Some(album.rating_key.as_str())).map(|n| n.idx);

        // Header: an ambient field from the cover's UltraBlur colors.
        let field = detail
            .as_ref()
            .and_then(|d| d.blur)
            .map(|b| mix(mix(b[0], b[1], 0.5), t.base, 0.35))
            .unwrap_or(mix(t.surface, t.discovery, 0.12));
        let on_field = mix(t.text_strong, field, 0.15);
        let total_ms: u64 = tracks.map(|v| v.iter().map(|t| t.duration_ms).sum()).unwrap_or(0);
        let genre = detail.as_ref().and_then(|d| d.genres.first().cloned());
        let kicker = ["ALBUM".to_string(), album.year.clone(), genre.unwrap_or_default().to_uppercase()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
        let badge = |s: String| {
            div()
                .px(px(6.0))
                .py(px(2.0))
                .rounded(px(4.0))
                .border_1()
                .border_color(gpui::hsla(0.0, 0.0, 1.0, 0.25))
                .font_family(MONO)
                .font_weight(FontWeight::MEDIUM)
                .text_size(px(11.0))
                .child(s)
        };
        let cover = match self.art.get(&album.thumb).cloned() {
            Some(i) => img(i).size(px(232.0)).rounded(px(8.0)).object_fit(ObjectFit::Cover).into_any_element(),
            None => div().size(px(232.0)).rounded(px(8.0)).bg(rgb(t.raised)).into_any_element(),
        };
        let (ak, an) = (album.artist_key.clone(), album.artist.clone());
        let header = div()
            .pt(px(28.0))
            .pb(px(28.0))
            .px(px(PAD_X))
            .flex()
            .gap(px(32.0))
            .items_end()
            .bg(linear_gradient(180.0, linear_color_stop(rgb(field), 0.0), linear_color_stop(rgb(t.base), 1.0)))
            .child(div().flex_none().shadow_2xl().child(cover))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .child(div().font_family(MONO).font_weight(FontWeight::MEDIUM).text_size(px(11.0)).text_color(rgb(on_field)).child(kicker))
                    .child(
                        div()
                            .text_size(px(if album.title.len() > 28 { 40.0 } else { 56.0 }))
                            .line_height(gpui::relative(1.05))
                            .font_weight(FontWeight::BOLD)
                            .text_color(rgb(t.text_strong))
                            .line_clamp(2)
                            .child(album.title.clone()),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(px(10.0))
                            .text_size(px(14.0))
                            .text_color(rgb(on_field))
                            .child(
                                div()
                                    .id("album-artist")
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(t.text_strong))
                                    .cursor_pointer()
                                    .hover(|d| d.underline())
                                    .child(album.artist.clone())
                                    .on_click(cx.listener(move |this, _, _, cx| this.open_artist(ak.clone(), an.clone(), cx))),
                            )
                            .when_some(tracks, |d, v| {
                                d.child("·").child(format!("{} tracks, {} min", v.len(), total_ms / 60_000))
                            })
                            .when_some(detail.as_ref().filter(|d| !d.format.is_empty()), |d, det| d.child("·").child(badge(det.format.clone())))
                            .when_some(detail.as_ref().and_then(|d| d.lufs), |d, l| d.child(badge(format!("{l:.1} LUFS").replace('-', "−")))),
                    ),
            );

        // Actions.
        let circle = |id: &'static str, ic: &'static str, color: u32| {
            div()
                .id(id)
                .size(px(44.0))
                .rounded(px(22.0))
                .border_1()
                .border_color(rgb(t.line))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|d| d.bg(rgb(t.raised)))
                .child(icons::icon(ic, 18.0, color))
        };
        let a1 = album.clone();
        let a2 = album.clone();
        let actions = div()
            .px(px(PAD_X))
            .pt(px(8.0))
            .pb(px(20.0))
            .flex()
            .items_center()
            .gap(px(12.0))
            .child(
                div()
                    .id("album-play")
                    .size(px(56.0))
                    .rounded(px(28.0))
                    .bg(rgb(t.accent))
                    .flex()
                    .items_center()
                    .justify_center()
                    .shadow_lg()
                    .cursor_pointer()
                    .hover(|d| d.opacity(0.9))
                    .child(icons::icon("play", 22.0, t.base))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(tr) = this.album_tracks() {
                            this.play_tracks(a1.clone(), tr, 0, cx);
                        }
                    })),
            )
            .child(circle("album-shuffle", "shuffle", t.text).on_click(cx.listener(move |this, _, _, cx| {
                if let Some(mut tr) = this.album_tracks() {
                    crate::shuffle(&mut tr);
                    this.start_queue(format!("{} (shuffled)", a2.title), None, tr, 0, cx);
                }
            })))
            .child(div().flex_1())
            .when(detail.is_some(), |d| d.child({
                let k = album.rating_key.clone();
                rate_stars(format!("ar-{k}"), detail.as_ref().and_then(|d| d.rating), 18.0, t.accent, t.line, cx, move |this, n, cx| this.rate_album(k.clone(), n, cx))
            }));

        // Track table.
        let head = div()
            .flex()
            .gap(px(12.0))
            .px(px(12.0))
            .pb(px(8.0))
            .border_b_1()
            .border_color(rgb(t.raised))
            .font_family(MONO)
            .font_weight(FontWeight::MEDIUM)
            .text_size(px(11.0))
            .text_color(rgb(t.muted))
            .child(div().w(px(40.0)).child("#"))
            .child(div().flex_1().child("TITLE"))
            .child(div().w(px(110.0)).child("RATING"))
            .child(div().w(px(56.0)).flex().justify_end().child("PLAYS"))
            .child(div().w(px(64.0)).flex().justify_end().child("TIME"));
        let rows: Vec<_> = tracks
            .map(|v| {
                v.iter()
                    .enumerate()
                    .map(|(i, tr)| {
                        let playing = playing_here == Some(i);
                        let (plays, rating) = detail.as_ref().and_then(|d| d.stats.get(&tr.rating_key).copied()).unwrap_or((0, None));
                        let album = album.clone();
                        div()
                            .id(("track", i))
                            .h(px(52.0))
                            .px(px(12.0))
                            .flex()
                            .items_center()
                            .gap(px(12.0))
                            .rounded(px(8.0))
                            .cursor_pointer()
                            .when(playing, |d| d.bg(rgb(t.raised)))
                            .hover(|d| d.bg(rgb(t.raised)))
                            .child(
                                div()
                                    .w(px(40.0))
                                    .font_family(MONO)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_size(px(13.0))
                                    .text_color(rgb(if playing { t.accent } else { t.muted }))
                                    .child(if playing { "▶".to_string() } else { (i + 1).to_string() }),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap(px(3.0))
                                    .child(
                                        div()
                                            .text_size(px(14.0))
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(rgb(if playing { t.accent } else { t.text }))
                                            .truncate()
                                            .child(tr.title.clone()),
                                    )
                                    .child(div().text_size(px(12.0)).text_color(rgb(t.muted)).truncate().child(tr.artist.clone())),
                            )
                            .child(div().w(px(110.0)).child({
                                let k = tr.rating_key.clone();
                                rate_stars(format!("tr-{k}"), rating, 13.0, t.accent, t.line, cx, move |this, n, cx| this.rate_track(k.clone(), n, cx))
                            }))
                            .child(
                                div()
                                    .w(px(56.0))
                                    .flex()
                                    .justify_end()
                                    .font_family(MONO)
                                    .text_size(px(12.0))
                                    .text_color(rgb(t.muted))
                                    .child(plays.to_string()),
                            )
                            .child(
                                div()
                                    .w(px(64.0))
                                    .flex()
                                    .justify_end()
                                    .font_family(MONO)
                                    .text_size(px(12.0))
                                    .text_color(rgb(t.text2))
                                    .child(fmt_ms(tr.duration_ms)),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(tr) = this.album_tracks() {
                                    this.play_tracks(album.clone(), tr, i, cx);
                                }
                            }))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let table = div().flex_1().min_w_0().flex().flex_col().child(head).children(rows);

        // About.
        let aside = detail.as_ref().map(|d| {
            let row = |k: &'static str, v: String| {
                div()
                    .flex()
                    .gap(px(12.0))
                    .text_size(px(12.0))
                    .child(div().w(px(90.0)).flex_none().text_color(rgb(t.muted)).child(k))
                    .child(div().flex_1().min_w_0().text_color(rgb(t.text)).child(v))
            };
            let mut facts = div().flex().flex_col().gap(px(6.0));
            if !d.released.is_empty() {
                facts = facts.child(row("Released", long_date(&d.released)));
            }
            if !d.studio.is_empty() {
                facts = facts.child(row("Label", d.studio.clone()));
            }
            if !d.styles.is_empty() {
                facts = facts.child(row("Styles", d.styles.iter().take(3).cloned().collect::<Vec<_>>().join(", ")));
            }
            if !d.moods.is_empty() {
                facts = facts.child(row("Moods", d.moods.iter().take(3).cloned().collect::<Vec<_>>().join(", ")));
            }
            if d.added_at > 0 {
                facts = facts.child(row("Added", month_year(d.added_at)));
            }
            div()
                .w(px(340.0))
                .flex_none()
                .flex()
                .flex_col()
                .gap(px(10.0))
                .child(div().text_size(px(15.0)).font_weight(FontWeight::SEMIBOLD).text_color(rgb(t.text_strong)).child("About this album"))
                .when(!d.summary.is_empty(), |el| {
                    el.child(div().text_size(px(13.0)).line_height(px(21.0)).text_color(rgb(t.text2)).line_clamp(8).child(d.summary.clone()))
                })
                .child(facts)
        });

        div()
            .id("album-view")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.album_scroll)
            .flex()
            .flex_col()
            .child(header)
            .child(actions)
            .child(div().px(px(PAD_X)).pb(px(40.0)).flex().gap(px(36.0)).child(table).children(aside))
            .into_any_element()
    }
}
