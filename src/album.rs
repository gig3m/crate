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
) -> gpui::Stateful<gpui::Div> {
    let id: gpui::SharedString = id.into();
    let full = rating.map(|r| (r / 2.0).floor() as usize).unwrap_or(0);
    // Rating sits inside clickable rows (play the track): clicks on the
    // stars, or between them, stay here.
    div()
        .id(gpui::SharedString::from(format!("{id}-stars")))
        .flex()
        .gap(px(2.0))
        .on_click(|_, _, cx| cx.stop_propagation())
        .children((0..5u8).map(|i| {
            let f = on_rate.clone();
            div()
                .id(gpui::SharedString::from(format!("{id}-{i}")))
                .cursor_pointer()
                .child(if (i as usize) < full { icons::icon("star", size, on) } else { icons::icon("star-outline", size, off) })
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    f(this, i + 1, cx)
                }))
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
    /// Other editions of `album` in the library: same artist, same title once
    /// bracketed suffixes ("(Deluxe Edition)", "[Remastered]") are dropped.
    /// (index, label) with "Original" for the plain title; empty unless 2+.
    pub(crate) fn editions(&self, album: &crate::plex::Album) -> Vec<(usize, String)> {
        let base = edition_base(&album.title);
        if base.is_empty() || album.artist_key.is_empty() {
            return Vec::new();
        }
        let mut out: Vec<(usize, String)> = self
            .albums
            .iter()
            .enumerate()
            .filter(|(_, a)| a.artist_key == album.artist_key && edition_base(&a.title) == base)
            .map(|(i, a)| (i, edition_label(&a.title)))
            .collect();
        if out.len() < 2 {
            return Vec::new();
        }
        // Same title twice (e.g. two "Nine Lives"): tell them apart by year,
        // when the years differ (track counts show beside them anyway).
        let snapshot: Vec<(String, String)> = out.iter().map(|(i, l)| (l.clone(), self.albums.get(*i).map(|a| a.year.clone()).unwrap_or_default())).collect();
        for (i, l) in out.iter_mut() {
            let years: std::collections::HashSet<&String> = snapshot.iter().filter(|(x, _)| x == l).map(|(_, y)| y).collect();
            if years.len() > 1
                && let Some(y) = self.albums.get(*i).map(|a| a.year.clone()).filter(|y| !y.is_empty())
            {
                *l = format!("{l} · {y}");
            }
        }
        out.sort_by(|a, b| (a.1 != "Original").cmp(&(b.1 != "Original")).then(a.1.cmp(&b.1)));
        out
    }

    /// Card formats for the editions of `album` not fetched yet.
    pub(crate) fn request_edition_formats(&mut self, album: &crate::plex::Album, cx: &mut Context<Self>) {
        // Fetch what's unknown, and retry a format that didn't come back last time.
        let keys: Vec<String> = self
            .editions(album)
            .iter()
            .filter_map(|(i, _)| self.albums.get(*i))
            .map(|a| a.rating_key.clone())
            .filter(|k| self.edition_formats.get(k).is_none_or(|(n, f)| f.is_empty() && *n > 0))
            .collect();
        if keys.is_empty() {
            return;
        }
        for k in &keys {
            // Placeholder (keeps a known count) so other pages don't fetch it twice.
            self.edition_formats.entry(k.clone()).or_insert((0, String::new()));
        }
        let pending = keys.clone();
        let rx = crate::plex_call(move |c| async move { c.edition_info(keys).await });
        cx.spawn(async move |this, cx| {
            let res = rx.await;
            this.update(cx, |this, cx| {
                // Drop the placeholders, then keep only real answers, so a failed or
                // empty fetch is retried the next time the album opens.
                for k in &pending {
                    if this.edition_formats.get(k).is_some_and(|(n, _)| *n == 0) {
                        this.edition_formats.remove(k);
                    }
                }
                if let Ok(Ok(found)) = res {
                    // Keep a known count even if the format timed out (that retries next open).
                    this.edition_formats.extend(found.into_iter().filter(|(_, n, f)| *n > 0 || !f.is_empty()).map(|(k, n, f)| (k, (n, f))));
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

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

    pub(crate) fn album_tracks(&self) -> Option<Vec<Track>> {
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
            None => gpui::img(self.mark(crate::art::placeholder_for(&album.thumb))).size(px(232.0)).rounded(px(8.0)).object_fit(gpui::ObjectFit::Cover).into_any_element(),
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
            .flex_wrap()
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
            .child({
                let a4 = album.clone();
                div()
                    .id("album-sonic")
                    .h(px(44.0))
                    .px(px(16.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .rounded(px(22.0))
                    .border_1()
                    .border_color(rgb(t.line))
                    .text_size(px(14.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(t.text))
                    .cursor_pointer()
                    .hover(|d| d.bg(rgb(t.raised)))
                    .child(icons::icon("sonic", 16.0, t.discovery))
                    .child("Sonically similar")
                    .on_click(cx.listener(move |this, _, _, cx| this.play_album_sonic_radio(a4.clone(), cx)))
            })
            .child(self.playlist_menu("album-add-playlist", vec![album.rating_key.clone()], album.title.clone(), cx))
            .child({
                // Keep offline: green when kept, progress while downloading.
                let kept = self.offline.index.get(&album.rating_key).cloned();
                let job = self.offline.job.as_ref().filter(|j| j.key == album.rating_key);
                let color = match &kept {
                    Some(k) if k.complete => t.healthy,
                    Some(_) => t.accent,
                    None => t.text,
                };
                let a3 = album.clone();
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(circle("album-offline", "offline", color).on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(tr) = this.album_tracks() {
                            this.toggle_offline(a3.clone(), tr, cx);
                        }
                    })))
                    .when_some(job.map(|j| (j.done_tracks, j.total_tracks, j.bytes.load(std::sync::atomic::Ordering::Relaxed))), |d, (dn, tot, b)| {
                        d.child(
                            div()
                                .font_family(MONO)
                                .text_size(px(12.0))
                                .text_color(rgb(t.muted))
                                .child(format!("{dn}/{tot} · {}", crate::offline::fmt_bytes(b))),
                        )
                    })
                    .when(job.is_none() && kept.as_ref().is_some_and(|k| !k.complete), |d| {
                        let queued = self.offline.queue.contains(&album.rating_key);
                        d.child(
                            div()
                                .font_family(MONO)
                                .text_size(px(12.0))
                                .text_color(rgb(if queued { t.muted } else { t.red }))
                                .child(if queued { "queued" } else { "failed: see Offline" }),
                        )
                    })
                    .when(kept.as_ref().is_some_and(|k| k.complete), |d| {
                        d.child(div().font_family(MONO).text_size(px(12.0)).text_color(rgb(t.healthy)).child("offline"))
                    })
            })
            .child(div().flex_1())
            .when(detail.is_some(), |d| d.child({
                let k = album.rating_key.clone();
                rate_stars(format!("ar-{k}"), detail.as_ref().and_then(|d| d.rating), 18.0, t.accent, t.line, cx, move |this, n, cx| this.rate_album(k.clone(), n, cx))
            }));

        // Track table. keys.rs (TRACK_HEAD_H/TRACK_ROW_H) mirrors this geometry.
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
            .child(div().w(px(24.0)))
            .child(div().w(px(56.0)).flex().justify_end().child("PLAYS"))
            .child(div().w(px(64.0)).flex().justify_end().child("TIME"));
        // Multi-disc albums get a header per disc.
        let headers = tracks.map(|v| disc_headers(v)).unwrap_or_default();
        let rows: Vec<gpui::AnyElement> = tracks
            .map(|v| {
                v.iter()
                    .enumerate()
                    .flat_map(|(i, tr)| {
                        let header = headers.get(i).copied().unwrap_or(false).then(|| {
                            // Fixed height: keys.rs mirrors it (DISC_HEAD_H).
                            div()
                                .h(px(crate::keys::DISC_HEAD_H))
                                .flex()
                                .items_end()
                                .pb(px(6.0))
                                .px(px(12.0))
                                .font_family(MONO)
                                .font_weight(FontWeight::MEDIUM)
                                .text_size(px(11.0))
                                .text_color(rgb(t.text2))
                                .child(format!("DISC {}", tr.disc.max(1)))
                                .into_any_element()
                        });
                        let num = if tr.track_no > 0 { tr.track_no as usize } else { i + 1 };
                        // The track's own artist, only where it isn't the album's.
                        let own_artist = (!tr.artist.is_empty() && tr.artist != album.artist).then(|| tr.artist.clone());
                        let playing = playing_here == Some(i);
                        let at_cursor = self.cursor == Some(i);
                        let (plays, rating) = detail.as_ref().and_then(|d| d.stats.get(&tr.rating_key).copied()).unwrap_or((0, None));
                        let album = album.clone();
                        let row = div()
                            .id(("track", i))
                            .h(px(52.0))
                            .px(px(12.0))
                            .flex()
                            .items_center()
                            .gap(px(12.0))
                            .rounded(px(8.0))
                            .cursor_pointer()
                            .when(playing || at_cursor, |d| d.bg(rgb(t.raised)))
                            .border_1()
                            .border_color(if at_cursor { rgb(t.text2).into() } else { gpui::transparent_black() })
                            .hover(|d| d.bg(rgb(t.raised)))
                            .child(
                                div()
                                    .w(px(40.0))
                                    .font_family(MONO)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_size(px(13.0))
                                    .text_color(rgb(if playing { t.accent } else { t.muted }))
                                    .child(if playing { "▶".to_string() } else { num.to_string() }),
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
                                    .children(own_artist.map(|a| div().text_size(px(12.0)).text_color(rgb(t.muted)).truncate().child(a))),
                            )
                            .child(div().w(px(110.0)).child({
                                let k = tr.rating_key.clone();
                                rate_stars(format!("tr-{k}"), rating, 13.0, t.accent, t.line, cx, move |this, n, cx| this.rate_track(k.clone(), n, cx))
                            }))
                            .child(self.playlist_menu_icon(("tr-pl", i), vec![tr.rating_key.clone()], tr.title.clone(), t.muted, cx))
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
                            }));
                        header.into_iter().chain(std::iter::once(row.into_any_element()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        // The track list failed to load: say so, with Retry, instead of an
        // empty table.
        let failed = (tracks.is_none()).then(|| self.album_error.as_ref().filter(|(k, _)| *k == album.rating_key).map(|(_, e)| e.clone())).flatten();
        let error_row = failed.map(|e| {
            div()
                .px(px(12.0))
                .py(px(16.0))
                .flex()
                .items_center()
                .gap(px(12.0))
                .child(div().flex_1().min_w_0().text_size(px(13.0)).text_color(rgb(t.text2)).child(format!("Couldn't load the tracks: {e}")))
                .child(
                    div()
                        .id("album-retry")
                        .px(px(12.0))
                        .h(px(30.0))
                        .flex()
                        .items_center()
                        .rounded(px(8.0))
                        .border_1()
                        .border_color(rgb(t.line))
                        .text_size(px(13.0))
                        .text_color(rgb(t.text))
                        .cursor_pointer()
                        .hover(|d| d.bg(rgb(t.raised)))
                        .child("Retry")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.load_album_tracks(ix, cx);
                            cx.notify();
                        })),
                )
        });
        let table = div().flex_1().min_w_0().flex().flex_col().child(head).children(rows).children(error_row);

        // About.
        // Sonically similar albums, other artists first (canvas: three across).
        let similar: Vec<crate::plex::Album> = self
            .sonic_albums
            .get(&album.rating_key)
            .cloned()
            .flatten()
            .map(|mut v| {
                v.sort_by_key(|x| x.artist == album.artist);
                v.into_iter().filter(|x| x.rating_key != album.rating_key).take(3).collect()
            })
            .unwrap_or_default();
        let similar_el = (!similar.is_empty()).then(|| {
            div()
                .flex()
                .flex_col()
                .gap(px(12.0))
                .child(div().font_family(MONO).font_weight(FontWeight::MEDIUM).text_size(px(11.0)).text_color(rgb(t.discovery)).child("SONICALLY SIMILAR"))
                .child(div().flex().gap(px(12.0)).children(similar.into_iter().enumerate().map(|(i, a)| {
                    let ix = self.albums.iter().position(|x| x.rating_key == a.rating_key);
                    div()
                        .id(("simalb", i))
                        .w(px(104.0))
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .when(ix.is_some(), |d| d.cursor_pointer())
                        .child(self.cover_or_mark(&a.thumb, &a.rating_key, 104.0, 6.0))
                        .child(div().text_size(px(12.0)).font_weight(FontWeight::SEMIBOLD).text_color(rgb(t.text_strong)).truncate().child(a.title.clone()))
                        .child(div().text_size(px(11.0)).text_color(rgb(t.muted)).truncate().child(a.artist.clone()))
                        .when_some(ix, |d, ix| d.on_click(cx.listener(move |this, _, _, cx| this.open_album(ix, cx))))
                })))
        });
        // Editions in your library (canvas): the other versions of this album.
        let editions = self.editions(&album);
        let editions_el = (!editions.is_empty()).then(|| {
            div()
                .flex()
                .flex_col()
                .gap(px(8.0))
                .child(div().font_family(MONO).font_weight(FontWeight::MEDIUM).text_size(px(11.0)).text_color(rgb(t.muted)).child("EDITIONS IN YOUR LIBRARY"))
                .children(editions.into_iter().map(|(i, label)| {
                    let a = self.albums.get(i).cloned();
                    let here = i == ix;
                    let (tracks, fmt) = a.as_ref().and_then(|a| self.edition_formats.get(&a.rating_key)).cloned().unwrap_or_default();
                    let text = match tracks {
                        0 => label,
                        1 => format!("{label} · 1 track"),
                        n => format!("{label} · {n} tracks"),
                    };
                    div()
                        .id(("edition", i))
                        .h(px(34.0))
                        .px(px(12.0))
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(10.0))
                        .rounded(px(8.0))
                        .border_1()
                        .border_color(rgb(if here { t.accent } else { t.raised }))
                        .when(here, |d| d.bg(rgb(t.raised)))
                        .text_size(px(13.0))
                        .text_color(rgb(if here { t.text_strong } else { t.text }))
                        .when(!here, |d| d.cursor_pointer().hover(|d| d.bg(rgb(t.raised))).on_click(cx.listener(move |this, _, _, cx| this.open_album(i, cx))))
                        .child(div().min_w_0().truncate().child(text))
                        .when(!fmt.is_empty(), |d| d.child(div().flex_none().font_family(MONO).text_size(px(11.0)).text_color(rgb(t.muted)).child(fmt)))
                }))
        });
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
                .children(editions_el)
                .children(similar_el)
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

/// Words that mark a release variant rather than a different record
/// (matched as whole words).
const EDITION_WORDS: [&str; 23] = [
    "deluxe", "edition", "editions", "version", "versions", "remaster", "remastered", "remasters", "anniversary", "expanded", "bonus",
    "special", "standard", "reissue", "reissued", "collector", "collectors", "collector's", "mono", "stereo", "explicit", "clean", "redux",
];
/// Edition words that are also ordinary title words: alone after " - " / ": "
/// they need another edition word or a number ("Songs: Clean" is a title).
const AMBIGUOUS: [&str; 6] = ["clean", "special", "mono", "stereo", "explicit", "standard"];
/// Words allowed alongside them in a " - " / ": " suffix ("20th Anniversary Edition").
const EDITION_FILLER: [&str; 12] = ["the", "and", "with", "track", "tracks", "us", "u.s.", "uk", "international", "japanese", "limited", "digital"];

/// Which track rows start a disc and get a DISC header (multi-disc albums
/// only; a missing disc number counts as disc 1).
pub(crate) fn disc_headers(tracks: &[Track]) -> Vec<bool> {
    let disc = |t: &Track| t.disc.max(1);
    let mut d: Vec<u32> = tracks.iter().map(disc).collect();
    d.sort_unstable();
    d.dedup();
    if d.len() < 2 {
        return vec![false; tracks.len()];
    }
    tracks.iter().enumerate().map(|(i, t)| i == 0 || disc(&tracks[i - 1]) != disc(t)).collect()
}

fn words(s: &str) -> Vec<String> {
    s.split(|c: char| c.is_whitespace() || matches!(c, ',' | '/' | '-' | '(' | ')' | '[' | ']' | '&' | '+'))
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase().replace('\u{2019}', "'"))
        .collect()
}

/// A bracketed group is an edition marker if it has an edition word.
fn bracket_is_edition(inner: &str) -> bool {
    words(inner).iter().any(|w| EDITION_WORDS.contains(&w.as_str()))
}

/// A dash/colon suffix must be only edition words, numbers ("2009",
/// "20th") and filler, so "TRON: Legacy Reconfigured" stays its own album.
fn suffix_is_edition(suffix: &str) -> bool {
    let ws = words(suffix);
    let edition: Vec<&String> = ws.iter().filter(|w| EDITION_WORDS.contains(&w.as_str())).collect();
    let numbered = ws.iter().any(|w| w.chars().any(|c| c.is_ascii_digit()));
    let only_ambiguous = edition.iter().all(|w| AMBIGUOUS.contains(&w.as_str()));
    !edition.is_empty()
        && (!only_ambiguous || edition.len() > 1 || numbered)
        && ws.iter().all(|w| EDITION_WORDS.contains(&w.as_str()) || EDITION_FILLER.contains(&w.as_str()) || w.chars().any(|c| c.is_ascii_digit()))
}

/// Split trailing edition markers off a title: "(Deluxe Edition)",
/// "[Remastered]", " - Remastered 2009", ": Special Edition". Only segments
/// with an edition word count, so "Weezer (Blue Album)" stays its own record.
/// Returns (base, markers in title order).
fn edition_split(title: &str) -> (String, Vec<String>) {
    let mut rest = title.trim().to_string();
    let mut marks = Vec::new();
    loop {
        // A trailing bracketed group, matched from its closing bracket.
        let bracket = rest.chars().last().filter(|c| matches!(c, ')' | ']')).and_then(|close| {
            let open = if close == ')' { '(' } else { '[' };
            let mut depth = 0;
            for (i, c) in rest.char_indices().rev() {
                if c == close {
                    depth += 1;
                } else if c == open {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
            }
            None
        });
        if let Some(i) = bracket {
            let inner = rest[i + 1..rest.len() - 1].trim().to_string();
            if bracket_is_edition(&inner) {
                marks.push(inner);
                rest = rest[..i].trim().to_string();
                continue;
            }
            break;
        }
        // A " - suffix" or ": suffix".
        if let Some((i, sep)) = [" - ", " \u{2013} ", ": "].iter().filter_map(|sep| rest.rfind(sep).map(|i| (i, sep.len()))).max_by_key(|(i, _)| *i) {
            let suffix = rest[i + sep..].trim().to_string();
            if suffix_is_edition(&suffix) && i > 0 {
                marks.push(suffix);
                rest = rest[..i].trim().to_string();
                continue;
            }
        }
        break;
    }
    marks.reverse();
    (rest.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase(), marks)
}

fn edition_base(title: &str) -> String {
    edition_split(title).0
}

/// What makes an edition different: its markers, or "Original".
fn edition_label(title: &str) -> String {
    let marks = edition_split(title).1;
    if marks.is_empty() { "Original".into() } else { marks.join(" · ") }
}

#[cfg(test)]
mod edition_tests {
    use super::{edition_base, edition_label};

    #[test]
    fn editions() {
        assert_eq!(edition_base("By the Way (Deluxe Edition)"), "by the way");
        assert_eq!(edition_base("By  the Way"), "by the way");
        assert_eq!(edition_label("By the Way (Deluxe Edition)"), "Deluxe Edition");
        assert_eq!(edition_label("Diorama [Remastered] (U.S. Version)"), "Remastered · U.S. Version");
        assert_eq!(edition_label("Hoobastank"), "Original");
        // Different records that only differ in brackets stay apart.
        assert_eq!(edition_base("Weezer (Blue Album)"), "weezer (blue album)");
        assert_ne!(edition_base("Weezer (Green Album)"), edition_base("Weezer"));
        assert_eq!(edition_base("(What's the Story) Morning Glory?"), "(what's the story) morning glory?");
        // Dash and colon suffixes.
        assert_eq!(edition_base("Abbey Road - Remastered 2009"), "abbey road");
        assert_eq!(edition_label("Abbey Road - Remastered 2009"), "Remastered 2009");
        assert_eq!(edition_base("Nevermind: Deluxe Edition"), "nevermind");
        // Unbalanced or non-edition brackets don't make a fake edition.
        assert_eq!(edition_label("Live (Live"), "Original");
        assert_eq!(edition_base("Live (Live"), "live (live");
        assert_eq!(edition_label("Favourite Worst Nightmare (Standard Version)"), "Standard Version");
        // Whole words only, and strict dash/colon suffixes.
        assert_eq!(edition_label("TRON: Legacy"), "Original");
        assert_eq!(edition_label("TRON: Legacy Reconfigured"), "Original");
        assert_eq!(edition_label("Album: The Conversion"), "Original");
        assert_eq!(edition_label("Album (Monochrome)"), "Original");
        assert_eq!(edition_label("Greatest Hits: Standards"), "Original");
        assert_eq!(edition_label("OK Computer - 20th Anniversary Edition"), "20th Anniversary Edition");
        assert_eq!(edition_label("Hoobastank (20th Anniversary Edition)"), "20th Anniversary Edition");
        assert_eq!(edition_label("Album: Collector\u{2019}s Edition"), "Collector\u{2019}s Edition");
        assert_eq!(edition_label("Album: Expanded & Remastered"), "Expanded & Remastered");
        assert_eq!(edition_label("Album \u{2013} Remastered"), "Remastered");
        assert_eq!(edition_label("Songs: Clean"), "Original");
        assert_eq!(edition_label("Live - Special"), "Original");
        assert_eq!(edition_label("Album - Special Edition"), "Special Edition");
    }
}
