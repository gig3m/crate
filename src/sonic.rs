//! Sonic features from the server's sonic analysis: similar albums and
//! artists, sonic mixes (nearest tracks), and Sonic Adventure (a path of
//! tracks between two, via /library/sections/N/computePath).

use gpui::{Context, FontWeight, div, linear_color_stop, linear_gradient, prelude::*, px, rgb};
use gpui_component::input::Input;

use crate::{Crate, PAD_X, View, icons, plex::{Album, SearchHit, Track}, shell::MONO, theme::{Theme, mix}};

impl Crate {
    pub(crate) fn request_sonic_albums(&mut self, key: String, cx: &mut Context<Self>) {
        if self.sonic_albums.contains_key(&key) {
            return;
        }
        self.sonic_albums.insert(key.clone(), None);
        let k = key.clone();
        let rx = crate::plex_call(move |c| async move { c.nearest_albums(&k, 12).await });
        cx.spawn(async move |this, cx| {
            let res = rx.await;
            this.update(cx, |this, cx| match res {
                Ok(Ok(list)) => {
                    for a in &list {
                        this.request_thumb(&a.thumb.clone());
                    }
                    this.sonic_albums.insert(key, Some(list));
                    cx.notify();
                }
                // Not cached: the next visit asks again.
                _ => {
                    this.sonic_albums.remove(&key);
                }
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn request_sonic_artists(&mut self, key: String, cx: &mut Context<Self>) {
        if self.sonic_artists.contains_key(&key) {
            return;
        }
        self.sonic_artists.insert(key.clone(), None);
        let k = key.clone();
        let rx = crate::plex_call(move |c| async move { c.nearest_artists(&k, 7).await });
        cx.spawn(async move |this, cx| {
            let res = rx.await;
            this.update(cx, |this, cx| match res {
                Ok(Ok(list)) => {
                    // Not the artist itself; photos by the thumb the cards look up.
                    let list: Vec<ArtistRef> = list.into_iter().filter(|(k, _, _)| *k != key).take(6).collect();
                    for (k, _, th) in &list {
                        // The card falls back to the bare thumb path when Plex gave none.
                        let th = if th.is_empty() { format!("/library/metadata/{k}/thumb") } else { th.clone() };
                        this.request_thumb(&th);
                        // An album cover each too (artist mix collages fill with these).
                        if let Some(t) = this.albums.iter().find(|a| a.artist_key == *k).map(|a| a.thumb.clone()) {
                            this.request_thumb(&t);
                        }
                    }
                    this.sonic_artists.insert(key, Some(list));
                    cx.notify();
                }
                _ => {
                    this.sonic_artists.remove(&key);
                }
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn request_artist_kinds(&mut self, key: String, cx: &mut Context<Self>) {
        if self.artist_kinds.contains_key(&key) {
            return;
        }
        self.artist_kinds.insert(key.clone(), None);
        let (k, section) = (key.clone(), crate::section());
        let rx = crate::plex_call(move |c| async move { c.artist_album_kinds(&section, &k).await });
        cx.spawn(async move |this, cx| {
            let res = rx.await;
            this.update(cx, |this, cx| match res {
                Ok(Ok(kinds)) => {
                    this.artist_kinds.insert(key, Some(kinds));
                    cx.notify();
                }
                other => {
                    if let Ok(Err(e)) = other {
                        eprintln!("[crate] album kinds for artist {key}: {e:#}");
                    }
                    this.artist_kinds.remove(&key);
                }
            })
            .ok();
        })
        .detach();
    }

    /// "Sonically similar": a mix page of the tracks that sound most like
    /// the album's opening track.
    pub(crate) fn open_album_sonic_mix(&mut self, album: Album, cx: &mut Context<Self>) {
        let title = format!("Sounds like {}", album.title);
        let desc = format!("Tracks from your library that sound like the opening track of {}", album.title);
        self.open_mix(format!("sonic:{}", album.rating_key), title, desc, crate::mix::MixSource::Sonic(album.rating_key), cx);
    }

    pub(crate) fn open_sonic(&mut self, cx: &mut Context<Self>) {
        if self.sonic.from.is_none() {
            self.sonic.from = self.now.as_ref().and_then(|n| n.tracks.get(n.idx).cloned());
        }
        if !matches!(self.view, View::Soon("sonic")) {
            self.go(View::Soon("sonic"));
        }
        cx.notify();
    }

    pub(crate) fn on_sonic_search(&mut self, q: String, cx: &mut Context<Self>) {
        self.sonic.query_gen += 1;
        let gen_id = self.sonic.query_gen;
        let q = q.trim().to_string();
        if q.is_empty() {
            self.sonic.hits.clear();
            cx.notify();
            return;
        }
        let section = crate::section();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_millis(150)).await;
            if this.update(cx, |this, _| this.sonic.query_gen != gen_id).unwrap_or(true) {
                return;
            }
            let rx = crate::plex_call(move |c| async move { c.search(&q, &section).await });
            let res = rx.await;
            this.update(cx, |this, cx| {
                if this.sonic.query_gen != gen_id {
                    return;
                }
                match res {
                    Ok(Ok(hits)) => {
                        this.sonic.hits = hits.into_iter().filter(|h| h.kind == "track").take(8).collect();
                        for h in this.sonic.hits.clone() {
                            this.request_thumb(&h.thumb);
                        }
                    }
                    // Never leave the previous query's tracks under the new text.
                    _ => {
                        this.sonic.hits.clear();
                        this.status = "Track search failed".into();
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// A search hit as a Track (enough for the path endpoints and display).
    fn hit_track(h: &SearchHit) -> Track {
        let (artist, album) = h.subtitle.split_once(" · ").map(|(a, b)| (a.to_string(), b.to_string())).unwrap_or((h.subtitle.clone(), String::new()));
        Track {
            rating_key: h.rating_key.clone(),
            title: h.title.clone(),
            artist,
            duration_ms: 0,
            part_key: String::new(),
            album_key: h.parent_key.clone(),
            album_title: album,
            thumb: h.thumb.clone(),
            ..Default::default()
        }
    }

    pub(crate) fn compute_sonic_path_pub(&mut self, cx: &mut Context<Self>) {
        self.compute_sonic_path(cx);
    }

    fn compute_sonic_path(&mut self, cx: &mut Context<Self>) {
        self.compute_sonic_path_inner(false, cx);
    }

    /// Reroll (canvas): a different path between the same two tracks.
    pub(crate) fn reroll_sonic_path(&mut self, cx: &mut Context<Self>) {
        // Only from a finished path (the button hides while one computes).
        if self.sonic.busy || self.sonic.path.as_ref().is_none_or(|p| p.len() < 3) {
            return;
        }
        self.compute_sonic_path_inner(true, cx);
    }

    fn compute_sonic_path_inner(&mut self, reroll: bool, cx: &mut Context<Self>) {
        let (Some(from), Some(to)) = (self.sonic.from.clone(), self.sonic.to.clone()) else { return };
        let current: Vec<String> = self.sonic.path.as_ref().map(|p| p.iter().map(|t| t.rating_key.clone()).collect()).unwrap_or_default();
        let previous = self.sonic.path.take();
        self.sonic.busy = true;
        self.sonic.path_gen += 1;
        let gen_id = self.sonic.path_gen;
        let section = crate::section();
        let rx = crate::plex_call(move |c| async move {
            if reroll {
                c.sonic_path_reroll(&section, &from.rating_key, &to.rating_key, current).await
            } else {
                c.sonic_path(&section, &from.rating_key, &to.rating_key).await
            }
        });
        cx.spawn(async move |this, cx| {
            let res = rx.await;
            this.update(cx, |this, cx| {
                if this.sonic.path_gen != gen_id {
                    return;
                }
                this.sonic.busy = false;
                match res {
                    Ok(Ok(path)) if !path.is_empty() => {
                        // Endpoints known only by key get their details from the path.
                        if this.sonic.from.as_ref().is_some_and(|f| f.title.is_empty()) {
                            this.sonic.from = path.first().cloned();
                        }
                        if this.sonic.to.as_ref().is_some_and(|t| t.title.is_empty()) {
                            this.sonic.to = path.last().cloned();
                        }
                        for t in &path {
                            this.request_thumb(&t.thumb.clone());
                        }
                        this.sonic.path = Some(path);
                    }
                    Ok(Ok(_)) => this.status = "No sonic path between those tracks".into(),
                    // A reroll that found nothing keeps the path you had.
                    Ok(Err(e)) if reroll => {
                        this.sonic.path = previous;
                        this.say(format!("Couldn't reroll: {e:#}"));
                    }
                    Ok(Err(e)) => this.status = format!("Sonic Adventure: {e:#}").into(),
                    Err(_) if reroll => this.sonic.path = previous,
                    Err(_) => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn endpoint(&self, id: &'static str, label: &'static str, t: &Theme, track: Option<&Track>) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .h(px(52.0))
            .min_w(px(240.0))
            .pl(px(6.0))
            .pr(px(14.0))
            .flex()
            .items_center()
            .gap(px(10.0))
            .rounded(px(12.0))
            .border_1()
            .border_color(rgb(t.line))
            .bg(rgb(t.raised))
            .child(match track {
                Some(tr) => self.cover_or_mark(&tr.thumb, &tr.rating_key, 40.0, 6.0),
                None => div().size(px(40.0)).rounded(px(6.0)).bg(rgb(t.surface)).into_any_element(),
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(div().font_family(MONO).font_weight(FontWeight::MEDIUM).text_size(px(10.0)).text_color(rgb(t.muted)).child(label))
                    .child(div().text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).text_color(rgb(t.text_strong)).child(match track {
                        Some(tr) => format!("{} · {}", tr.title, tr.artist),
                        None => "Pick a track below".into(),
                    })),
            )
    }

    pub(crate) fn sonic_view(&self, t: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let s = &self.sonic;
        let path_cards: Vec<_> = s
            .path
            .clone()
            .unwrap_or_default()
            .into_iter()
            .enumerate()
            .map(|(i, tr)| {
                div()
                    .w(px(96.0))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap(px(5.0))
                    .child(self.cover_or_mark(&tr.thumb, &tr.rating_key, 96.0, 6.0))
                    .child(div().text_size(px(12.0)).font_weight(FontWeight::SEMIBOLD).text_color(rgb(t.text_strong)).truncate().child(tr.title.clone()))
                    .child(div().text_size(px(11.0)).text_color(rgb(t.muted)).truncate().child(tr.artist.clone()))
                    .id(("step", i))
            })
            .collect();
        let pill = |id: &'static str, label: String, primary: bool| {
            div()
                .id(id)
                .h(px(40.0))
                .px(px(18.0))
                .flex()
                .items_center()
                .rounded(px(20.0))
                .text_size(px(13.0))
                .font_weight(FontWeight::SEMIBOLD)
                .cursor_pointer()
                .when(primary, |d| d.bg(rgb(t.accent)).text_color(rgb(t.base)).hover(|d| d.opacity(0.9)))
                .when(!primary, |d| d.border_1().border_color(rgb(t.line)).text_color(rgb(t.text)).hover(|d| d.bg(rgb(t.raised))))
                .child(label)
        };
        let hits: Vec<_> = s
            .hits
            .iter()
            .cloned()
            .enumerate()
            .map(|(i, h)| {
                let (h1, h2) = (h.clone(), h.clone());
                div()
                    .id(("shit", i))
                    .h(px(48.0))
                    .px(px(10.0))
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .rounded(px(8.0))
                    .hover(|d| d.bg(rgb(t.raised)))
                    .child(self.cover_or_mark(&h.thumb, &h.rating_key, 36.0, 4.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().text_size(px(13.0)).text_color(rgb(t.text_strong)).truncate().child(h.title.clone()))
                            .child(div().text_size(px(12.0)).text_color(rgb(t.muted)).truncate().child(h.subtitle.clone())),
                    )
                    .child(pill_small(("from", i), "From", t).on_click(cx.listener(move |this, _, _, cx| {
                        this.sonic.from = Some(Self::hit_track(&h1));
                        this.compute_sonic_path(cx);
                        cx.notify();
                    })))
                    .child(pill_small(("to", i), "To", t).on_click(cx.listener(move |this, _, _, cx| {
                        this.sonic.to = Some(Self::hit_track(&h2));
                        this.compute_sonic_path(cx);
                        cx.notify();
                    })))
            })
            .collect();
        let path = s.path.clone();
        let ready = path.as_ref().is_some_and(|p| !p.is_empty());
        let status = if s.busy {
            "Walking the sonic path…".to_string()
        } else if let Some(p) = &path {
            format!("{} steps", p.len())
        } else if s.from.is_some() && s.to.is_some() {
            String::new()
        } else {
            "Pick where to start and where to end up.".into()
        };
        let now_track = self.now.as_ref().and_then(|n| n.tracks.get(n.idx).cloned());

        div()
            .id("sonic")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px(px(PAD_X))
            .pt(px(4.0))
            .pb(px(40.0))
            .flex()
            .flex_col()
            .gap(px(22.0))
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(14.0))
                    .child(div().text_size(px(30.0)).font_weight(FontWeight::BOLD).text_color(rgb(t.text_strong)).child("Sonic Adventure"))
                    .child(div().text_size(px(14.0)).text_color(rgb(t.muted)).child("A playlist that travels between two tracks, step by sonic step")),
            )
            .child(
                div()
                    .rounded(px(16.0))
                    .p(px(18.0))
                    .bg(linear_gradient(120.0, linear_color_stop(rgb(mix(t.surface, t.discovery, 0.12)), 0.0), linear_color_stop(rgb(t.surface), 0.7)))
                    .border_1()
                    .border_color(rgb(t.raised))
                    .flex()
                    .flex_col()
                    .gap(px(14.0))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(px(12.0))
                            .child(self.endpoint("sonic-from", "FROM", t, s.from.as_ref()))
                            .child(icons::icon("forward", 20.0, t.discovery))
                            .child(self.endpoint("sonic-to", "TO", t, s.to.as_ref()))
                            .child(div().flex_1())
                            .when_some(now_track.filter(|nt| s.from.as_ref().is_none_or(|f| f.rating_key != nt.rating_key)), |d, nt| {
                                d.child(pill("sonic-now", "Start from what's playing".into(), false).on_click(cx.listener(move |this, _, _, cx| {
                                    this.sonic.from = Some(nt.clone());
                                    this.compute_sonic_path(cx);
                                    cx.notify();
                                })))
                            })
                            .when(ready, |d| {
                                let p = path.clone().unwrap_or_default();
                                let (from, to) = (s.from.clone(), s.to.clone());
                                // Rerolling needs a middle to detour around.
                                d.when(p.len() >= 3, |d| {
                                    d.child(pill("sonic-reroll", "Reroll path".into(), false).on_click(cx.listener(|this, _, _, cx| {
                                        this.reroll_sonic_path(cx);
                                        cx.notify();
                                    })))
                                })
                                .child(pill("sonic-play", "Play adventure".into(), true).on_click(cx.listener(move |this, _, _, cx| {
                                    let name = match (&from, &to) {
                                        (Some(a), Some(b)) => format!("Sonic Adventure: {} → {}", a.title, b.title),
                                        _ => "Sonic Adventure".into(),
                                    };
                                    this.start_queue(name, None, p.clone(), 0, cx);
                                })))
                            }),
                    )
                    .child(div().font_family(MONO).text_size(px(12.0)).text_color(rgb(t.muted)).child(status))
                    .when(!path_cards.is_empty(), |d| d.child(div().flex().flex_wrap().gap(px(10.0)).children(path_cards))),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(div().text_size(px(18.0)).font_weight(FontWeight::SEMIBOLD).text_color(rgb(t.text_strong)).child("Find a track"))
                    .child(
                        div()
                            .h(px(38.0))
                            .max_w(px(520.0))
                            .px(px(4.0))
                            .rounded(px(8.0))
                            .border_1()
                            .border_color(rgb(t.border))
                            .bg(rgb(t.surface))
                            .child(Input::new(&self.sonic_search).appearance(false)),
                    )
                    .children(hits),
            )
            .into_any_element()
    }
}

fn pill_small(id: (&'static str, usize), label: &'static str, t: &Theme) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .h(px(28.0))
        .px(px(12.0))
        .flex()
        .items_center()
        .rounded(px(14.0))
        .border_1()
        .border_color(rgb(t.discovery))
        .text_size(px(12.0))
        .font_weight(FontWeight::MEDIUM)
        .text_color(rgb(t.discovery))
        .cursor_pointer()
        .hover(|d| d.bg(rgb(t.raised)))
        .child(label)
}

/// (artist key, name, photo)
pub type ArtistRef = (String, String, String);
/// (singles & EPs, live) album keys.
pub type AlbumKinds = (std::collections::HashSet<String>, std::collections::HashSet<String>);

#[derive(Default)]
pub struct SonicState {
    pub from: Option<Track>,
    pub to: Option<Track>,
    pub path: Option<Vec<Track>>,
    pub busy: bool,
    pub hits: Vec<SearchHit>,
    pub query_gen: u64,
    pub path_gen: u64,
}

