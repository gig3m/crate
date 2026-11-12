//! Mixes as pages: Home's artist and mood mixes and the artist page's Mix
//! builder open a page with the batch of tracks Plex picked, Play / Save
//! as playlist / New batch, and the track list. Radio-backed mixes keep
//! going after the batch (the queue refills from the same station).

use gpui::{Context, FontWeight, ObjectFit, div, img, linear_color_stop, linear_gradient, prelude::*, px, rgb};

use crate::{Crate, PAD_X, View, fmt_ms, icons, plex::Track, shell::MONO, theme::{Theme, mix}};

/// Where a mix's tracks come from.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) enum MixSource {
    /// Plex radio seeded from an artist (or track): refills as it plays.
    Radio(String),
    /// A station key or `query:<filter>` (mood mixes): refills as it plays.
    Station(String),
    /// Mix builder: an artist's most-played tracks interleaved with the
    /// similar artists' (a fixed batch).
    Builder { key: String, similar: bool },
}

/// Tracks a radio mix page gathers before showing (Plex hands out artist
/// radio five at a time).
const RADIO_BATCH: usize = 25;

pub(crate) struct MixPage {
    pub title: String,
    pub desc: String,
    pub source: MixSource,
    /// The station this page's batch came from, which the queue refills
    /// from when it plays (radio and mood mixes).
    pub station: Option<String>,
    pub tracks: Option<Vec<Track>>,
    pub error: Option<String>,
    /// Bumped per load, so a stale reply is dropped.
    pub load_gen: u64,
}

impl Crate {
    /// Open a mix's page, loading its first batch (a page opened before
    /// keeps the batch it had).
    pub(crate) fn open_mix(&mut self, id: String, title: String, desc: String, source: MixSource, cx: &mut Context<Self>) {
        let fresh = match self.mixes.get(&id) {
            Some(p) => p.source != source || (p.tracks.is_none() && p.error.is_some()),
            None => true,
        };
        if fresh {
            // Keep counting loads, so a reply for the page replaced is dropped.
            let load_gen = self.mixes.get(&id).map(|p| p.load_gen).unwrap_or(0);
            self.mixes.insert(id.clone(), MixPage { title, desc, source, station: None, tracks: None, error: None, load_gen });
            self.load_mix(id.clone(), cx);
        } else if let Some(p) = self.mixes.get_mut(&id) {
            p.title = title;
            p.desc = desc;
        }
        self.now_playing = false;
        self.go(View::Mix { id });
        cx.notify();
    }

    /// Fetch a new batch for the mix.
    pub(crate) fn load_mix(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(p) = self.mixes.get_mut(&id) else { return };
        p.load_gen += 1;
        p.tracks = None;
        p.error = None;
        let (load_gen, source) = (p.load_gen, p.source.clone());
        match source {
            MixSource::Builder { key, similar } => {
                let mut keys = vec![key.clone()];
                if let Some(Some(sim)) = self.sonic_artists.get(&key).filter(|_| similar) {
                    keys.extend(sim.iter().take(3).map(|s| s.0.clone()));
                } else {
                    self.request_sonic_artists(key.clone(), cx);
                }
                let section = crate::section();
                let rx = crate::plex_call(move |c| async move { c.artist_mix(&section, keys, 8).await });
                cx.spawn(async move |this, cx| {
                    let res = rx.await;
                    this.update(cx, |this, cx| {
                        this.mix_loaded(&id, load_gen, None, res.map_err(|_| anyhow::anyhow!("cancelled")).and_then(|r| r), cx)
                    })
                    .ok();
                })
                .detach();
            }
            MixSource::Radio(seed) => {
                let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_micros()).unwrap_or(0);
                let station = format!("/library/metadata/{seed}/station/crate-{nonce:x}?type=10");
                self.fetch_mix_batch(id, load_gen, station, cx);
            }
            MixSource::Station(station) => self.fetch_mix_batch(id, load_gen, station, cx),
        }
        cx.notify();
    }

    /// Gather a radio batch: post to the station until RADIO_BATCH new
    /// tracks (or it stops giving new ones).
    fn fetch_mix_batch(&mut self, id: String, load_gen: u64, station: String, cx: &mut Context<Self>) {
        let key = station.clone();
        let rx = crate::plex_call(move |c| async move {
            let mut out: Vec<Track> = Vec::new();
            for _ in 0..8 {
                let more = match key.strip_prefix("query:") {
                    Some(filter) => c.random_tracks(&crate::section(), filter, RADIO_BATCH).await?,
                    None => c.play_queue(&key).await.map(|(_, t)| t)?,
                };
                let before = out.len();
                for t in more {
                    if !out.iter().any(|o| o.rating_key == t.rating_key) {
                        out.push(t);
                    }
                }
                if out.len() >= RADIO_BATCH || out.len() == before {
                    break;
                }
            }
            out.truncate(RADIO_BATCH);
            anyhow::Ok(out)
        });
        cx.spawn(async move |this, cx| {
            let res = rx.await;
            this.update(cx, |this, cx| this.mix_loaded(&id, load_gen, Some(station), res.map_err(|_| anyhow::anyhow!("cancelled")).and_then(|r| r), cx))
                .ok();
        })
        .detach();
    }

    fn mix_loaded(&mut self, id: &str, load_gen: u64, station: Option<String>, res: anyhow::Result<Vec<Track>>, cx: &mut Context<Self>) {
        let Some(p) = self.mixes.get_mut(id) else { return };
        if p.load_gen != load_gen {
            return;
        }
        match res {
            Ok(tracks) if !tracks.is_empty() => {
                for t in &tracks {
                    self.request_thumb(&t.thumb);
                }
                let p = self.mixes.get_mut(id).expect("checked");
                p.tracks = Some(tracks);
                p.station = station;
            }
            Ok(_) => p.error = Some("Plex returned no tracks for this mix".into()),
            Err(e) => p.error = Some(format!("{e:#}")),
        }
        cx.notify();
    }

    /// The mix playing now, by page id.
    fn playing_mix(&self) -> Option<&str> {
        self.now.as_ref().and_then(|n| n.context_key.as_deref()).and_then(|k| k.strip_prefix("mix:"))
    }

    /// Tracks the page lists: the live queue while this mix plays (it grows
    /// as radio refills), else the page's batch.
    fn mix_tracks(&self, id: &str) -> Vec<Track> {
        if self.playing_mix() == Some(id)
            && let Some(n) = self.now.as_ref()
        {
            return n.tracks.clone();
        }
        self.mixes.get(id).and_then(|p| p.tracks.clone()).unwrap_or_default()
    }

    /// Play the mix from track `i`; radio mixes keep going from their station.
    pub(crate) fn play_mix_from(&mut self, id: &str, i: usize, cx: &mut Context<Self>) {
        if self.playing_mix() == Some(id) {
            self.play_from(i, 0, cx);
            return;
        }
        let Some(p) = self.mixes.get(id) else { return };
        let Some(tracks) = p.tracks.clone().filter(|t| !t.is_empty()) else { return };
        let (title, station) = (p.title.clone(), p.station.clone());
        self.play_intent += 1;
        self.start_queue(title, Some(format!("mix:{id}")), tracks, i, cx);
        if let Some(n) = self.now.as_mut() {
            n.refill = station;
        }
    }

    /// Save these tracks as a new regular playlist (the naming bar opens
    /// with `title`).
    pub(crate) fn save_tracks_as_playlist(&mut self, tracks: &[Track], title: String, window: &mut gpui::Window, cx: &mut Context<Self>) {
        let keys: Vec<String> = tracks.iter().map(|t| t.rating_key.clone()).collect();
        if keys.is_empty() {
            return;
        }
        self.start_naming(crate::playlist_add::NameFor::Tracks(keys), title, window, cx);
    }

    pub(crate) fn mix_view(&self, id: &str, t: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(p) = self.mixes.get(id) else { return div().into_any_element() };
        let tracks = self.mix_tracks(id);
        let playing_here = (self.playing_mix() == Some(id)).then(|| self.now.as_ref().map(|n| n.idx)).flatten();
        let radio = !matches!(p.source, MixSource::Builder { .. });

        // A 2x2 collage of the batch's covers (distinct albums first).
        let mut covers: Vec<String> = Vec::new();
        for tr in &tracks {
            if covers.len() < 4 && !tr.thumb.is_empty() && !covers.contains(&tr.thumb) {
                covers.push(tr.thumb.clone());
            }
        }
        while covers.len() < 4 {
            covers.push(format!("{}-{}", p.title, covers.len()));
        }
        let half = 99.0;
        let collage = div()
            .size(px(200.0))
            .flex_none()
            .flex()
            .flex_wrap()
            .gap(px(2.0))
            .rounded(px(8.0))
            .overflow_hidden()
            .shadow_2xl()
            .children(covers.iter().map(|k| self.cover_or_mark(k, k, half, 0.0)));

        let facts = match (&p.tracks, &p.error) {
            (_, Some(e)) if tracks.is_empty() => format!("Couldn't load: {e}"),
            (None, _) if tracks.is_empty() => "Picking tracks…".into(),
            _ => {
                let mins = tracks.iter().map(|t| t.duration_ms).sum::<u64>() / 60_000;
                let len = if mins >= 60 { format!("{} h {} min", mins / 60, mins % 60) } else { format!("{mins} min") };
                let more = if radio { " · keeps going with more like these" } else { "" };
                format!("{} tracks, {len}{more}", tracks.len())
            }
        };
        let field = mix(t.surface, t.discovery, 0.14);
        let header = div()
            .pt(px(24.0))
            .pb(px(20.0))
            .px(px(PAD_X))
            .flex()
            .gap(px(28.0))
            .items_end()
            .bg(linear_gradient(180.0, linear_color_stop(rgb(field), 0.0), linear_color_stop(rgb(t.base), 1.0)))
            .child(collage)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .child(div().font_family(MONO).font_weight(FontWeight::MEDIUM).text_size(px(11.0)).text_color(rgb(t.text2)).child("MIX"))
                    .child(
                        div()
                            .text_size(px(if p.title.len() > 28 { 36.0 } else { 52.0 }))
                            .line_height(gpui::relative(1.05))
                            .font_weight(FontWeight::BOLD)
                            .text_color(rgb(t.text_strong))
                            .line_clamp(2)
                            .child(p.title.clone()),
                    )
                    .when(!p.desc.is_empty(), |d| d.child(div().text_size(px(14.0)).text_color(rgb(t.text)).child(p.desc.clone())))
                    .child(div().text_size(px(14.0)).text_color(rgb(t.text2)).child(facts)),
            );

        let pill = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .h(px(34.0))
                .px(px(14.0))
                .flex()
                .items_center()
                .rounded(px(17.0))
                .border_1()
                .border_color(rgb(t.line))
                .text_size(px(13.0))
                .text_color(rgb(t.text))
                .cursor_pointer()
                .hover(|d| d.bg(rgb(t.raised)))
                .child(label)
        };
        let has = !tracks.is_empty();
        let (id1, id2, id3, id4) = (id.to_string(), id.to_string(), id.to_string(), id.to_string());
        let save_title = p.title.clone();
        let actions = div()
            .px(px(PAD_X))
            .pb(px(16.0))
            .flex()
            .items_center()
            .gap(px(12.0))
            .when(has, |d| {
                d.child(
                    div()
                        .id("mix-play")
                        .size(px(52.0))
                        .rounded(px(26.0))
                        .bg(rgb(t.accent))
                        .flex()
                        .items_center()
                        .justify_center()
                        .shadow_lg()
                        .cursor_pointer()
                        .hover(|d| d.opacity(0.9))
                        .child(icons::icon(if playing_here.is_some() && self.now.as_ref().is_some_and(|n| !n.paused && !n.finished) { "pause" } else { "play" }, 22.0, t.base))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if this.playing_mix() == Some(id1.as_str()) {
                                this.transport(crate::mpris::Transport::PlayPause, cx);
                            } else {
                                this.play_mix_from(&id1, 0, cx);
                            }
                        })),
                )
            })
            .child(div().flex_1())
            .child(self.pin_pill(
                "mix-pin",
                crate::pins::Pin { kind: crate::pins::PinKind::Mix, key: id.to_string(), title: p.title.clone(), thumb: covers.first().cloned().unwrap_or_default(), mix: Some(p.source.clone()), desc: p.desc.clone() },
                t,
                cx,
            ))
            .when(has, |d| {
                d.child(pill("mix-save", "Save as playlist").on_click(cx.listener(move |this, _, window, cx| {
                    let tracks = this.mix_tracks(&id2);
                    this.save_tracks_as_playlist(&tracks, save_title.clone(), window, cx);
                })))
            })
            .when(playing_here.is_none() && (has || p.error.is_some()), |d| {
                let label = if p.error.is_some() && !has { "Retry" } else if radio { "New batch" } else { "Rebuild" };
                d.child(pill("mix-new", label).on_click(cx.listener(move |this, _, _, cx| this.load_mix(id3.clone(), cx))))
            });

        let rows = tracks.iter().enumerate().map(|(i, tr)| {
            let art = match self.art.get(&tr.thumb).cloned() {
                Some(im) => img(im).size(px(36.0)).rounded(px(4.0)).object_fit(ObjectFit::Cover).into_any_element(),
                None => gpui::img(self.mark(crate::art::placeholder_for(&tr.thumb))).size(px(36.0)).rounded(px(4.0)).object_fit(ObjectFit::Cover).into_any_element(),
            };
            let playing = playing_here == Some(i);
            let played = playing_here.is_some_and(|c| i < c);
            let id = id4.clone();
            div()
                .id(("mix-track", i))
                .h(px(52.0))
                .w_full()
                .px(px(12.0))
                .flex()
                .items_center()
                .gap(px(12.0))
                .rounded(px(8.0))
                .cursor_pointer()
                .when(playing, |d| d.bg(rgb(t.raised)))
                .when(played, |d| d.opacity(0.55))
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
                        .items_center()
                        .gap(px(12.0))
                        .child(div().flex_none().child(art))
                        .child(
                            div()
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
                        ),
                )
                .child(div().w(px(220.0)).text_size(px(13.0)).text_color(rgb(t.text2)).truncate().child(tr.album_title.clone()))
                .child(div().w(px(64.0)).flex().justify_end().font_family(MONO).text_size(px(13.0)).text_color(rgb(t.muted)).child(fmt_ms(tr.duration_ms)))
                .child(div().w(px(28.0)).flex().justify_center().child(self.playlist_menu_icon(("mix-add", i), vec![tr.rating_key.clone()], tr.title.clone(), t.muted, cx)))
                .on_click(cx.listener(move |this, _, _, cx| this.play_mix_from(&id, i, cx)))
        });

        let head = div()
            .mx(px(PAD_X))
            .px(px(12.0))
            .pb(px(8.0))
            .flex()
            .gap(px(12.0))
            .font_family(MONO)
            .font_weight(FontWeight::MEDIUM)
            .text_size(px(11.0))
            .text_color(rgb(t.muted))
            .border_b_1()
            .border_color(rgb(t.line))
            .child(div().w(px(40.0)).child("#"))
            .child(div().flex_1().child("TITLE"))
            .child(div().w(px(220.0)).child("ALBUM"))
            .child(div().w(px(64.0)).flex().justify_end().child("TIME"))
            .child(div().w(px(28.0)));

        div()
            .id("mix-page")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.album_scroll)
            .child(header)
            .child(actions)
            .when(has, |d| d.child(head).child(div().px(px(PAD_X)).pt(px(6.0)).pb(px(40.0)).flex().flex_col().children(rows)))
            .into_any_element()
    }
}
