//! Artist page from the design canvas (Artist.dc.html).

use gpui::{Context, FontWeight, ObjectFit, div, img, linear_color_stop, linear_gradient, prelude::*, px, rgb};

use crate::{Crate, PAD_X, icons, plex::{Album, ArtistInfo}, shell::MONO, theme::{Theme, mix}};

/// `0xRRGGBB` with alpha 0..1.
fn rgba(c: u32, a: f32) -> gpui::Hsla {
    gpui::rgba((c << 8) | (a.clamp(0.0, 1.0) * 255.0) as u32).into()
}

impl Crate {
    /// Start Plex radio seeded from an artist, track or album. Plex has
    /// stations for artists and tracks (`/library/metadata/{key}/station/...`)
    /// but none for albums (an empty queue), so an album seeds from its first track.
    pub(crate) fn play_radio(&mut self, key: &str, title: String, cx: &mut Context<Self>) {
        if let Some(album) = self.albums.iter().find(|a| a.rating_key == key).cloned() {
            self.play_intent += 1;
            let intent = self.play_intent;
            self.status = format!("Starting {title}…").into();
            cx.notify();
            self.with_tracks(album, cx, move |this, _album, tracks, cx| {
                if this.play_intent != intent {
                    return;
                }
                match tracks.first() {
                    Some(first) => {
                        let k = first.rating_key.clone();
                        this.play_station_key(&k, title, cx);
                    }
                    None => {
                        this.status = "That album has no tracks to start radio from".into();
                        cx.notify();
                    }
                }
            });
            return;
        }
        self.play_station_key(key, title, cx);
    }

    fn play_station_key(&mut self, key: &str, title: String, cx: &mut Context<Self>) {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_micros()).unwrap_or(0);
        self.play_station(format!("/library/metadata/{key}/station/crate-{nonce:x}?type=10"), title, cx);
    }

    pub(crate) fn artist_view(&self, key: &str, name: &str, info: Option<&ArtistInfo>, t: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let name = if name.is_empty() { info.map(|i| i.title.clone()).unwrap_or_default() } else { name.to_string() };
        let mut albums: Vec<(usize, Album)> =
            self.albums.iter().enumerate().filter(|(_, a)| a.artist_key == key).map(|(i, a)| (i, a.clone())).collect();
        albums.sort_by(|a, b| b.1.year.cmp(&a.1.year));

        // Header: two-layer ambient field from the photo's UltraBlur colors.
        let blur = info.and_then(|i| i.blur);
        let (c1, c2) = match blur {
            Some(b) => (mix(b[1], t.base, 0.35), mix(b[0], t.base, 0.2)),
            None => (mix(t.surface, t.accent, 0.12), mix(t.surface, t.discovery, 0.12)),
        };
        let on_field = mix(t.text_strong, c1, 0.12);
        let photo = match info.and_then(|i| self.art.get(&i.thumb).cloned()) {
            Some(i) => img(i).size(px(160.0)).rounded(px(80.0)).object_fit(ObjectFit::Cover).into_any_element(),
            None => gpui::img(self.mark(crate::art::placeholder_for(&name))).size(px(160.0)).rounded(px(80.0)).object_fit(gpui::ObjectFit::Cover).into_any_element(),
        };
        let kicker = std::iter::once("ARTIST".to_string())
            .chain(info.map(|i| i.genres.iter().take(2).map(|g| g.to_uppercase()).collect::<Vec<_>>()).unwrap_or_default())
            .collect::<Vec<_>>()
            .join(" · ");
        let stats = match info {
            Some(i) if i.plays > 0 => format!("{} albums in your library · {} plays", albums.len(), i.plays),
            _ => format!("{} albums in your library", albums.len()),
        };
        let header = div()
            .h(px(300.0))
            .flex_none()
            .relative()
            .bg(linear_gradient(180.0, linear_color_stop(rgb(c1), 0.0), linear_color_stop(rgb(t.base), 1.0)))
            .child(div().absolute().inset_0().bg(linear_gradient(90.0, linear_color_stop(rgba(c2, 0.55), 0.0), linear_color_stop(rgba(c2, 0.0), 0.6))))
            .child(
                div()
                    .absolute()
                    .left(px(PAD_X))
                    .right(px(PAD_X))
                    .bottom(px(28.0))
                    .flex()
                    .items_end()
                    .gap(px(28.0))
                    .child(div().flex_none().rounded(px(83.0)).border_3().border_color(rgb(t.base)).shadow_2xl().child(photo))
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(8.0))
                            .child(div().font_family(MONO).font_weight(FontWeight::MEDIUM).text_size(px(11.0)).text_color(rgb(on_field)).child(kicker))
                            .child(
                                div()
                                    .text_size(px(if name.len() > 22 { 44.0 } else { 64.0 }))
                                    .line_height(gpui::relative(1.0))
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(rgb(t.text_strong))
                                    .truncate()
                                    .child(name.clone()),
                            )
                            .child(div().text_size(px(14.0)).text_color(rgb(on_field)).child(stats)),
                    ),
            );

        // Actions: play top tracks, shuffle everything, artist radio.
        let pill = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .h(px(44.0))
                .px(px(18.0))
                .flex()
                .items_center()
                .rounded(px(22.0))
                .border_1()
                .border_color(rgb(t.line))
                .text_size(px(14.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(rgb(t.text))
                .cursor_pointer()
                .hover(|d| d.bg(rgb(t.raised)))
                .child(label)
        };
        let all: Vec<_> = info.map(|i| i.popular.iter().map(|p| p.0.clone()).collect()).unwrap_or_default();
        let (all1, all2) = (all.clone(), all);
        let (n1, n2, n3, k3) = (name.clone(), name.clone(), name.clone(), key.to_string());
        let (n4, k4) = (name.clone(), key.to_string());
        let actions = div()
            .px(px(PAD_X))
            .pt(px(16.0))
            .pb(px(24.0))
            .flex()
            .items_center()
            .gap(px(12.0))
            .child(
                div()
                    .id("artist-play")
                    .size(px(56.0))
                    .rounded(px(28.0))
                    .bg(rgb(t.accent))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(|d| d.opacity(0.9))
                    .child(icons::icon("play", 22.0, t.base))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !all1.is_empty() {
                            this.start_queue(format!("{n1}: most played"), None, all1.clone(), 0, cx);
                        }
                    })),
            )
            .child(pill("artist-shuffle", "Shuffle all").on_click(cx.listener(move |this, _, _, cx| {
                let mut v = all2.clone();
                if !v.is_empty() {
                    crate::shuffle(&mut v);
                    this.start_queue(format!("{n2} (shuffled)"), None, v, 0, cx);
                }
            })))
            .child(pill("artist-radio", "Artist radio").on_click(cx.listener(move |this, _, _, cx| {
                this.play_radio(&k3, format!("{n3} radio"), cx);
            })))
            .child(pill("artist-mix", "Mix builder").on_click(cx.listener(move |this, _, _, cx| this.build_artist_mix(k4.clone(), n4.clone(), cx))))
            .child(self.pin_pill(
                "artist-pin",
                crate::pins::Pin { kind: crate::pins::PinKind::Artist, key: key.to_string(), title: name.clone(), thumb: info.map(|i| i.thumb.clone()).unwrap_or_default(), mix: None, desc: String::new() },
                t,
                cx,
            ));

        // Popular in your library: top five with a play-count bar.
        let top = info.map(|i| i.popular.iter().take(5).cloned().collect::<Vec<_>>()).unwrap_or_default();
        let max_plays = top.iter().map(|p| p.3).max().unwrap_or(1).max(1);
        let popular: Vec<_> = top
            .into_iter()
            .enumerate()
            .map(|(n, (tr, album_key, album_title, plays))| {
                let tk = tr.rating_key.clone();
                let cover = match self.art.get(&tr.thumb).cloned() {
                    Some(i) => img(i).size(px(44.0)).flex_none().rounded(px(4.0)).object_fit(ObjectFit::Cover).into_any_element(),
                    None => gpui::img(self.mark(crate::art::placeholder_for(&tr.thumb))).size(px(44.0)).flex_none().rounded(px(4.0)).object_fit(gpui::ObjectFit::Cover).into_any_element(),
                };
                div()
                    .id(("pop", n))
                    .h(px(56.0))
                    .px(px(10.0))
                    .flex()
                    .items_center()
                    .gap(px(14.0))
                    .rounded(px(8.0))
                    .cursor_pointer()
                    .hover(|d| d.bg(rgb(t.raised)))
                    .child(div().w(px(28.0)).font_family(MONO).font_weight(FontWeight::MEDIUM).text_size(px(13.0)).text_color(rgb(t.muted)).child((n + 1).to_string()))
                    .child(cover)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .child(div().text_size(px(14.0)).font_weight(FontWeight::MEDIUM).text_color(rgb(t.text_strong)).truncate().child(tr.title.clone()))
                            .child(div().text_size(px(12.0)).text_color(rgb(t.muted)).truncate().child(album_title.clone())),
                    )
                    .child(
                        div()
                            .w(px(90.0))
                            .h(px(4.0))
                            .rounded(px(2.0))
                            .bg(rgb(t.raised))
                            .child(div().h_full().rounded(px(2.0)).w(gpui::relative(plays as f32 / max_plays as f32)).bg(rgb(t.accent))),
                    )
                    .child(div().w(px(60.0)).flex().justify_end().font_family(MONO).text_size(px(12.0)).text_color(rgb(t.text2)).child(plays.to_string()))
                    .on_click(cx.listener(move |this, _, _, cx| this.play_track_in_album(&album_key, tk.clone(), cx)))
            })
            .collect();

        // Albums: five columns at the canvas width.
        let main_w = (self.content_w - 340.0 - 40.0).max(400.0);
        let cols = ((main_w + 20.0) / (150.0 + 20.0)).floor().clamp(2.0, 5.0) as usize;
        let card_w = ((main_w - 20.0 * (cols as f32 - 1.0)) / cols as f32).floor();
        // Discography tabs (canvas): Albums / Singles & EPs / Live, from the
        // library's Format and Subformat tags (plus "Live at…" titles, since
        // live albums are often untagged). Only tabs with albums show.
        let kinds = self.artist_kinds.get(key).cloned().flatten();
        let kind_of = |a: &Album| -> usize {
            let t = a.title.to_lowercase();
            let live_title = t == "live"
                || ["live at ", "live from ", "live in "].iter().any(|p| t.starts_with(p))
                || ["(live)", "[live]", "(live at", "(live from", "(live in"].iter().any(|p| t.contains(p))
                || t.ends_with(" - live");
            match &kinds {
                Some((small, _)) if small.contains(&a.rating_key) => 1,
                Some((_, live)) if live.contains(&a.rating_key) => 2,
                _ if live_title => 2,
                _ => 0,
            }
        };
        let counts = albums.iter().fold([0usize; 3], |mut c, (_, a)| {
            c[kind_of(a)] += 1;
            c
        });
        // This artist's chosen tab if it still has albums, else the first that does.
        let want = self.artist_tabs.get(key).copied().unwrap_or(0).min(2);
        let tab = if counts[want] > 0 { want } else { (0..3).find(|&i| counts[i] > 0).unwrap_or(0) };
        let artist_key = key.to_string();
        let tabs_el = (counts[1] + counts[2] > 0).then(|| {
            div().flex().gap(px(4.0)).p(px(4.0)).rounded(px(10.0)).bg(rgb(t.surface)).border_1().border_color(rgb(t.raised)).children(
                ["Albums", "Singles & EPs", "Live"].iter().enumerate().filter(|(i, _)| counts[*i] > 0).map(|(i, label)| {
                    let on = i == tab;
                    div()
                        .id(("dtab", i))
                        .h(px(32.0))
                        .px(px(12.0))
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .rounded(px(7.0))
                        .text_size(px(13.0))
                        .font_weight(FontWeight::MEDIUM)
                        .bg(rgb(if on { t.raised } else { t.surface }))
                        .text_color(rgb(if on { t.text_strong } else { t.text2 }))
                        .cursor_pointer()
                        .child(*label)
                        .child(div().font_family(MONO).text_size(px(11.0)).text_color(rgb(t.muted)).child(counts[i].to_string()))
                        .on_click({
                            let k = artist_key.clone();
                            cx.listener(move |this, _, _, cx| {
                                this.artist_tabs.insert(k.clone(), i);
                                cx.notify();
                            })
                        })
                }),
            )
        });
        let cards: Vec<_> = albums
            .iter()
            .filter(|(_, a)| kind_of(a) == tab)
            .map(|(ix, a)| {
                let ix = *ix;
                let cover = match self.art.get(&a.thumb).cloned() {
                    Some(i) => img(i).size(px(card_w)).rounded(px(8.0)).object_fit(ObjectFit::Cover).into_any_element(),
                    None => gpui::img(self.mark(crate::art::placeholder_for(&a.thumb))).size(px(card_w)).rounded(px(8.0)).object_fit(gpui::ObjectFit::Cover).into_any_element(),
                };
                div()
                    .id(("aa", ix))
                    .w(px(card_w))
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .cursor_pointer()
                    .child(cover)
                    .child(div().text_size(px(14.0)).font_weight(FontWeight::SEMIBOLD).text_color(rgb(t.text_strong)).truncate().child(a.title.clone()))
                    .child(div().font_family(MONO).text_size(px(11.0)).text_color(rgb(t.muted)).child(a.year.clone()))
                    .on_click(cx.listener(move |this, _, _, cx| this.open_album(ix, cx)))
            })
            .collect();
        let h2 = |s: &'static str| div().text_size(px(20.0)).font_weight(FontWeight::SEMIBOLD).text_color(rgb(t.text_strong)).child(s);
        let left = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(32.0))
            .when(!popular.is_empty(), |d| d.child(div().flex().flex_col().gap(px(8.0)).child(h2("Popular in your library")).children(popular)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(14.0))
                    .child(div().flex().items_center().justify_between().child(h2(["Albums", "Singles & EPs", "Live"][tab])).children(tabs_el))
                    .child(div().flex().flex_wrap().gap(px(20.0)).children(cards)),
            );

        // Aside: similar artists you have, biography.
        // Sonically similar artists when the server has them, else Plex's
        // metadata "similar" tags that match artists in the library.
        let sonic_thumbs: std::collections::HashMap<String, String> =
            self.sonic_artists.get(key).cloned().flatten().unwrap_or_default().into_iter().map(|(k, _, th)| (k, th)).collect();
        let sonic: Vec<(String, String)> =
            self.sonic_artists.get(key).cloned().flatten().unwrap_or_default().into_iter().map(|(k, n, _)| (k, n)).collect();
        let sonic_label = !sonic.is_empty();
        let tagged: Vec<(String, String)> = info
            .map(|i| {
                i.similar
                    .iter()
                    .filter_map(|s| self.albums.iter().find(|a| a.artist.eq_ignore_ascii_case(s)).map(|a| (a.artist_key.clone(), a.artist.clone())))
                    .take(6)
                    .collect()
            })
            .unwrap_or_default();
        let similar = if sonic_label { sonic } else { tagged };
        let label = |s: &'static str, c: u32| div().font_family(MONO).font_weight(FontWeight::MEDIUM).text_size(px(11.0)).text_color(rgb(c)).child(s);
        let sim_cards: Vec<_> = similar
            .into_iter()
            .enumerate()
            .map(|(i, (k, n))| {
                let thumb = sonic_thumbs.get(&k).filter(|t| !t.is_empty()).cloned().unwrap_or_else(|| format!("/library/metadata/{k}/thumb"));
                let face = match self.art.get(&thumb).cloned() {
                    Some(im) => img(im).size(px(92.0)).rounded(px(46.0)).object_fit(ObjectFit::Cover).into_any_element(),
                    None => gpui::img(self.mark(crate::art::placeholder_for(&thumb))).size(px(92.0)).rounded(px(46.0)).object_fit(gpui::ObjectFit::Cover).into_any_element(),
                };
                let n2 = n.clone();
                div()
                    .id(("sim", i))
                    .w(px(96.0))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(8.0))
                    .cursor_pointer()
                    .child(face)
                    .child(div().w_full().text_size(px(12.0)).font_weight(FontWeight::MEDIUM).text_color(rgb(t.text)).text_center().line_clamp(2).child(n))
                    .on_click(cx.listener(move |this, _, _, cx| this.open_artist(k.clone(), n2.clone(), cx)))
            })
            .collect();
        // Appears on (canvas): albums by others with tracks credited to this artist.
        let appears: Vec<(usize, Album)> = self
            .appears_on
            .get(key)
            .map(|v| v.iter().filter_map(|k| self.albums.iter().position(|a| a.rating_key == *k).map(|ix| (ix, self.albums[ix].clone()))).take(6).collect())
            .unwrap_or_default();
        let aside = div()
            .w(px(340.0))
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(28.0))
            .when(!sim_cards.is_empty(), |d| {
                d.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(14.0))
                        .child(label(if sonic_label { "SONICALLY SIMILAR ARTISTS" } else { "SIMILAR ARTISTS IN YOUR LIBRARY" }, t.discovery))
                        .child(div().flex().flex_wrap().gap(px(14.0)).children(sim_cards)),
                )
            })
            .when(!appears.is_empty(), |d| {
                d.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(10.0))
                        .child(label("APPEARS ON", t.muted))
                        .children(appears.into_iter().map(|(i, a)| {
                            div()
                                .id(("appears", i))
                                .flex()
                                .items_center()
                                .gap(px(12.0))
                                .cursor_pointer()
                                .rounded(px(8.0))
                                .hover(|d| d.bg(rgb(t.raised)))
                                .child(self.cover_or_mark(&a.thumb, &a.rating_key, 44.0, 4.0))
                                .child(
                                    div()
                                        .min_w_0()
                                        .flex()
                                        .flex_col()
                                        .gap(px(2.0))
                                        .child(div().text_size(px(13.0)).font_weight(FontWeight::MEDIUM).text_color(rgb(t.text_strong)).truncate().child(a.title.clone()))
                                        .child(div().text_size(px(12.0)).text_color(rgb(t.muted)).truncate().child(if a.year.is_empty() { a.artist.clone() } else { format!("{} · {}", a.artist, a.year) })),
                                )
                                .on_click(cx.listener(move |this, _, _, cx| this.open_album(i, cx)))
                        })),
                )
            })
            .when_some(info.filter(|i| !i.summary.is_empty()), |d, i| {
                d.child(
                    div()
                        .p(px(18.0))
                        .rounded(px(12.0))
                        .bg(rgb(t.surface))
                        .border_1()
                        .border_color(rgb(t.raised))
                        .flex()
                        .flex_col()
                        .gap(px(10.0))
                        .child(div().text_size(px(15.0)).font_weight(FontWeight::SEMIBOLD).text_color(rgb(t.text_strong)).child("Biography"))
                        .child(div().text_size(px(13.0)).line_height(px(21.0)).text_color(rgb(t.text2)).line_clamp(12).child(i.summary.clone()))
                        .child(div().text_size(px(12.0)).text_color(rgb(t.muted)).child("Metadata from your Plex server agent")),
                )
            });

        div()
            .id("artist-view")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.album_scroll)
            .flex()
            .flex_col()
            .child(header)
            .child(actions)
            .child(div().px(px(PAD_X)).pb(px(40.0)).flex().gap(px(40.0)).child(left).child(aside))
            .into_any_element()
    }

    /// Appears on for `key`, fetched in the background (bounded 3 s) so the
    /// artist page never waits on search; cached per artist.
    pub(crate) fn request_appears_on(&mut self, key: String, name: String, cx: &mut Context<Self>) {
        if self.appears_on.contains_key(&key) {
            return;
        }
        self.appears_on.insert(key.clone(), Vec::new());
        let section = crate::section();
        let k = key.clone();
        let rx = crate::plex_call(move |c| async move {
            tokio::time::timeout(std::time::Duration::from_secs(3), c.appears_on(&k, &name, &section)).await.map_err(|_| anyhow::anyhow!("timed out"))?
        });
        cx.spawn(async move |this, cx| {
            let res = rx.await;
            this.update(cx, |this, cx| {
                match res {
                    Ok(Ok(v)) => {
                        this.appears_on.insert(key.clone(), v);
                        this.request_appears_art(&key);
                    }
                    // Failed: forget it so the next visit tries again.
                    _ => {
                        this.appears_on.remove(&key);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Covers for an artist's Appears on albums (when they or the album list arrive).
    pub(crate) fn request_appears_art(&mut self, key: &str) {
        let thumbs: Vec<String> =
            self.appears_on.get(key).map(|v| v.iter().filter_map(|k| self.albums.iter().find(|a| a.rating_key == *k)).take(6).map(|a| a.thumb.clone()).collect()).unwrap_or_default();
        for th in thumbs {
            self.request_thumb(&th);
        }
    }

    /// Mix builder (canvas): a page of this artist's most-played tracks
    /// interleaved with those of the three most sonically similar artists.
    pub(crate) fn build_artist_mix(&mut self, key: String, name: String, cx: &mut Context<Self>) {
        let with_others = self.sonic_artists.get(&key).is_some_and(|s| s.as_ref().is_some_and(|s| !s.is_empty()));
        let (title, desc) = if with_others {
            (format!("{name} + similar mix"), format!("{name}'s most-played tracks, mixed with similar artists from your library"))
        } else {
            (format!("{name} mix"), format!("{name}'s most-played tracks"))
        };
        self.open_mix(format!("builder:{key}{}", if with_others { "+similar" } else { "" }), title, desc, crate::mix::MixSource::Builder { key, similar: with_others }, cx);
    }

    /// Art the artist page needs once its info arrives.
    pub(crate) fn request_artist_art(&mut self, info: &ArtistInfo) {
        self.request_thumb(&info.thumb.clone());
        let thumbs: Vec<String> = info.popular.iter().take(5).map(|p| p.0.thumb.clone()).collect();
        for th in thumbs {
            self.request_thumb(&th);
        }
        let keys: Vec<String> = info
            .similar
            .iter()
            .filter_map(|s| self.albums.iter().find(|a| a.artist.eq_ignore_ascii_case(s)).map(|a| a.artist_key.clone()))
            .take(6)
            .collect();
        for k in keys {
            self.request_thumb(&format!("/library/metadata/{k}/thumb"));
        }
    }
}
