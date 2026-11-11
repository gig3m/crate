//! Full-window Now Playing from the design canvas (NowPlaying.dc.html):
//! ambient field from the album colors, cover and transport, synced
//! lyrics, and a Queue / Signal path / Track info panel.

use gpui::{Context, FontWeight, Hsla, ObjectFit, div, img, linear_color_stop, linear_gradient, prelude::*, px, rgb};

use crate::{Crate, fmt_ms, icons, mpris::Transport, shell::MONO, theme::{Theme, mix}};

fn white(a: f32) -> Hsla {
    gpui::hsla(0.0, 0.0, 1.0, a)
}

fn rgba(c: u32, a: f32) -> Hsla {
    gpui::rgba((c << 8) | (a.clamp(0.0, 1.0) * 255.0) as u32).into()
}

fn khz(r: u32) -> String {
    if r.is_multiple_of(1000) { format!("{}", r / 1000) } else { format!("{:.1}", r as f32 / 1000.0) }
}

fn day(ts: i64) -> String {
    if ts <= 0 {
        return "—".into();
    }
    let days = ts.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    const M: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    format!("{} {d}, {y}", M[(m - 1) as usize])
}

impl Crate {
    pub(crate) fn now_playing_view(&self, t: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(n) = self.now.as_ref() else {
            return div().size_full().bg(rgb(t.base)).into_any_element();
        };
        let tr = &n.tracks[n.idx];
        let detail = self.track_detail.get(&tr.rating_key).cloned().flatten();
        let blur = self.album_detail.get(&tr.album_key).cloned().flatten().and_then(|d| d.blur);
        let (c1, c2) = match blur {
            Some(b) => (mix(b[1], t.base, 0.45), mix(b[3], t.base, 0.25)),
            None => (mix(t.base, t.discovery, 0.25), mix(t.base, t.accent, 0.2)),
        };
        let paused = n.paused || n.finished;

        // Top row.
        let kind = if n.context_key.is_some() {
            "PLAYING FROM ALBUM"
        } else if n.refill.is_some() {
            "PLAYING FROM RADIO"
        } else {
            "PLAYING FROM QUEUE"
        };
        let top = div()
            .flex()
            .items_center()
            .gap(px(14.0))
            .child(
                div()
                    .id("np-collapse")
                    .size(px(38.0))
                    .rounded(px(10.0))
                    .bg(white(0.08))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(|d| d.bg(white(0.14)))
                    .child(icons::icon("down", 18.0, 0xffffff))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.now_playing = false;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(div().font_family(MONO).font_weight(FontWeight::MEDIUM).text_size(px(11.0)).text_color(white(0.72)).child(kind))
                    .child(div().text_size(px(14.0)).font_weight(FontWeight::SEMIBOLD).text_color(white(1.0)).child(n.context.clone())),
            );

        // Left: cover, title, waveform, transport.
        let cover = match self.art.get(&tr.thumb).cloned() {
            Some(i) => img(i).size(px(420.0)).rounded(px(12.0)).object_fit(ObjectFit::Cover).into_any_element(),
            None => gpui::img(self.mark(crate::art::placeholder_for(&tr.thumb))).size(px(420.0)).rounded(px(12.0)).object_fit(gpui::ObjectFit::Cover).into_any_element(),
        };
        let len = tr.duration_ms;
        let frac = if len > 0 { (n.elapsed_ms as f32 / len as f32).min(1.0) } else { 0.0 };
        const BARS: usize = 80;
        let levels = detail.as_ref().map(|d| d.levels.clone()).unwrap_or_default();
        let bars: Vec<_> = (0..BARS)
            .map(|i| {
                let lv = if levels.is_empty() { None } else { levels.get(i * levels.len() / BARS).copied() };
                let h = lv.map(|v| (4.0 + 36.0 * v).round()).unwrap_or(4.0);
                let ms = len * i as u64 / BARS as u64;
                let played = (i as f32 + 0.5) / BARS as f32 <= frac;
                div()
                    .id(("np-seek", i))
                    .flex_1()
                    .h(px(40.0))
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .child(div().w_full().h(px(h)).rounded(px(1.0)).bg(if played { white(1.0) } else { white(0.22) }))
                    .on_click(cx.listener(move |this, _, _, cx| this.transport(Transport::SetPosition(ms), cx)))
            })
            .collect();
        let np_stars = {
            let k = tr.rating_key.clone();
            crate::album::rate_stars(format!("np-{k}"), detail.as_ref().and_then(|d| d.rating), 18.0, t.accent, 0x8a8a8a, cx, move |this, n, cx| {
                this.rate_track(k.clone(), n, cx)
            })
        };
        let btn = |id: &'static str, ic: &'static str, size: f32, color: Hsla, tr: Option<Transport>| {
            div()
                .id(id)
                .size(px(44.0))
                .flex()
                .items_center()
                .justify_center()
                .child(gpui::svg().path(format!("icons/{ic}.svg")).size(px(size)).text_color(color))
                .when_some(tr, |d, tr| d.cursor_pointer().on_click(cx.listener(move |this, _, _, cx| this.transport(tr, cx))))
        };
        let (ak, an) = (self.albums.iter().find(|a| a.rating_key == tr.album_key).map(|a| a.artist_key.clone()), tr.artist.clone());
        let left = div()
            .w(px(420.0))
            .flex_none()
            .flex()
            .flex_col()
            .justify_center()
            .gap(px(18.0))
            .child(div().shadow_2xl().rounded(px(12.0)).child(cover))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_start()
                    .gap(px(12.0))
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(4.0))
                            .child(div().text_size(px(28.0)).font_weight(FontWeight::BOLD).text_color(white(1.0)).line_clamp(2).child(tr.title.clone()))
                            .child(
                                div()
                                    .id("np-artist")
                                    .text_size(px(16.0))
                                    .text_color(white(0.8))
                                    .cursor_pointer()
                                    .hover(|d| d.underline())
                                    .child(tr.artist.clone())
                                    .when_some(ak, |d, ak| {
                                        d.on_click(cx.listener(move |this, _, _, cx| {
                                            this.now_playing = false;
                                            this.open_artist(ak.clone(), an.clone(), cx);
                                        }))
                                    }),
                            ),
                    )
                    .child(div().pt(px(8.0)).child(np_stars)),
            )
            .child(div().flex().gap(px(2.0)).children(bars))
            .child(
                div()
                    .mt(px(-10.0))
                    .flex()
                    .justify_between()
                    .font_family(MONO)
                    .text_size(px(12.0))
                    .text_color(white(0.72))
                    .child(fmt_ms(n.elapsed_ms))
                    .child(format!("−{}", fmt_ms(len.saturating_sub(n.elapsed_ms)))),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(px(26.0))
                    .child(btn("np-shuffle", "shuffle", 20.0, white(0.3), None))
                    .child(btn("np-prev", "prev", 26.0, white(1.0), Some(Transport::Prev)))
                    .child(
                        div()
                            .id("np-pp")
                            .size(px(68.0))
                            .rounded(px(34.0))
                            .bg(white(1.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .shadow_lg()
                            .cursor_pointer()
                            .child(icons::icon(if paused { "play" } else { "pause" }, 26.0, mix(c1, 0x000000, 0.6)))
                            .on_click(cx.listener(|this, _, _, cx| this.transport(Transport::PlayPause, cx))),
                    )
                    .child(btn("np-next", "next", 26.0, white(1.0), Some(Transport::Next)))
                    .child(btn("np-repeat", "repeat", 20.0, white(0.3), None)),
            );

        // Middle: synced lyrics around the current line.
        let lyrics = detail.as_ref().map(|d| d.lyrics.clone()).unwrap_or_default();
        let lyric_col = if lyrics.is_empty() {
            let msg = if detail.is_some() { "No lyrics for this track" } else { "" };
            div().flex_1().min_w_0().flex().items_center().text_size(px(15.0)).text_color(white(0.5)).child(msg)
        } else {
            let cur = lyrics.iter().rposition(|l| l.0 <= n.elapsed_ms).unwrap_or(0);
            let start = cur.saturating_sub(3);
            let lines: Vec<_> = lyrics
                .iter()
                .enumerate()
                .skip(start)
                .filter(|(_, l)| !l.1.is_empty())
                .take(8)
                .map(|(i, l)| {
                    let (size, alpha) = if i == cur {
                        (34.0, 1.0)
                    } else if i == cur + 1 {
                        (28.0, 0.68)
                    } else {
                        (28.0, 0.34)
                    };
                    let ms = l.0;
                    div()
                        .id(("lyric", i))
                        .text_size(px(size))
                        .font_weight(FontWeight::BOLD)
                        .line_height(gpui::relative(1.25))
                        .text_color(white(alpha))
                        .cursor_pointer()
                        .child(l.1.clone())
                        .on_click(cx.listener(move |this, _, _, cx| this.transport(Transport::SetPosition(ms), cx)))
                })
                .collect();
            let by = detail.as_ref().map(|d| d.lyrics_by.clone()).unwrap_or_default();
            div()
                .flex_1()
                .min_w_0()
                .px(px(8.0))
                .flex()
                .flex_col()
                .justify_center()
                .gap(px(18.0))
                .children(lines)
                .child(
                    div()
                        .font_family(MONO)
                        .text_size(px(11.0))
                        .text_color(white(0.62))
                        .child(if by.contains("lyricfind") { "Synced lyrics via Plex (LyricFind)" } else { "Synced lyrics via Plex" }),
                )
        };

        // Right: Queue / Signal path / Track info.
        let tabs = div().flex().gap(px(4.0)).p(px(4.0)).rounded(px(10.0)).bg(white(0.06)).children(
            ["Queue", "Signal path", "Track info"].iter().enumerate().map(|(i, label)| {
                let on = self.np_tab == i;
                div()
                    .id(("np-tab", i))
                    .flex_1()
                    .h(px(32.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(7.0))
                    .text_size(px(13.0))
                    .font_weight(FontWeight::MEDIUM)
                    .bg(if on { white(0.14) } else { white(0.0) })
                    .text_color(if on { white(1.0) } else { white(0.72) })
                    .cursor_pointer()
                    .child(*label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.np_tab = i;
                        cx.notify();
                    }))
            }),
        );
        let panel_body = match self.np_tab {
            0 => {
                let radio = n.refill.is_some();
                let rows: Vec<_> = n
                    .tracks
                    .iter()
                    .enumerate()
                    .skip(n.idx + 1)
                    .take(9)
                    .map(|(i, q)| {
                        let art = match self.art.get(&q.thumb).cloned() {
                            Some(im) => img(im).size(px(40.0)).flex_none().rounded(px(4.0)).object_fit(ObjectFit::Cover).into_any_element(),
                            None => gpui::img(self.mark(crate::art::placeholder_for(&q.thumb))).size(px(40.0)).flex_none().rounded(px(4.0)).object_fit(gpui::ObjectFit::Cover).into_any_element(),
                        };
                        div()
                            .id(("q", i))
                            .px(px(6.0))
                            .py(px(7.0))
                            .flex()
                            .items_center()
                            .gap(px(12.0))
                            .rounded(px(8.0))
                            .cursor_pointer()
                            .hover(|d| d.bg(white(0.06)))
                            .child(art)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap(px(2.0))
                                    .child(div().text_size(px(13.0)).font_weight(FontWeight::MEDIUM).text_color(white(1.0)).truncate().child(q.title.clone()))
                                    .child(div().text_size(px(12.0)).text_color(white(0.66)).truncate().child(q.artist.clone())),
                            )
                            .when(radio, |d| d.child(div().font_family(MONO).text_size(px(10.0)).text_color(rgb(t.healthy)).child("RADIO")))
                            .when(!radio && self.dj_keys.contains(&(n.queue_id, q.rating_key.clone())), |d| d.child(div().font_family(MONO).text_size(px(10.0)).text_color(rgb(t.discovery)).child("DJ")))
                            .child(div().font_family(MONO).text_size(px(11.0)).text_color(white(0.66)).child(fmt_ms(q.duration_ms)))
                            .on_click(cx.listener(move |this, _, _, cx| this.play_from(i, 0, cx)))
                    })
                    .collect();
                let label = if radio { "NEXT FROM RADIO" } else { "NEXT UP" };
                let dj_on = self.guest_dj;
                // Guest DJ toggle (canvas): not offered on radio, which is sonic already.
                let dj = (!radio).then(|| {
                    div()
                        .id("guest-dj")
                        .h(px(22.0))
                        .px(px(9.0))
                        .flex()
                        .items_center()
                        .rounded(px(11.0))
                        .border_1()
                        .border_color(if dj_on { rgb(t.discovery).into() } else { white(0.2) })
                        .font_family(MONO)
                        .text_size(px(10.0))
                        .text_color(if dj_on { rgb(t.discovery).into() } else { white(0.66) })
                        .cursor_pointer()
                        .child(if dj_on { "GUEST DJ · ON" } else { "GUEST DJ" })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.guest_dj = !this.guest_dj;
                            this.say(if this.guest_dj { "Guest DJ on: a sonically nearby track after each one" } else { "Guest DJ off" });
                            if this.guest_dj {
                                this.guest_dj_tick(cx);
                            }
                            cx.notify();
                        }))
                });
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(
                        div()
                            .px(px(6.0))
                            .py(px(4.0))
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(div().font_family(MONO).font_weight(FontWeight::MEDIUM).text_size(px(11.0)).text_color(white(0.66)).child(label))
                            .children(dj),
                    )
                    .when(rows.is_empty(), |d| d.child(div().px(px(6.0)).text_size(px(13.0)).text_color(white(0.6)).child("End of queue")))
                    .children(rows)
            }
            1 => {
                let fmt = n.formats.get(&n.idx).copied();
                let codec = detail.as_ref().map(|d| d.codec.clone()).filter(|c| !c.is_empty()).unwrap_or_else(|| "Audio".into());
                let tx = n.transcoded.get(&n.idx).copied();
                let source = match fmt {
                    // The decoder sees Plex's MP3, not the file: describe the file.
                    _ if tx.is_some() => {
                        let kbps = detail.as_ref().and_then(|d| d.bitrate).map(|b| format!(" · {b} kbps")).unwrap_or_default();
                        format!("{codec} on the server{kbps}")
                    }
                    Some((rate, bits)) => {
                        let kbps = detail.as_ref().and_then(|d| d.bitrate).map(|b| format!(" · {b} kbps")).unwrap_or_default();
                        format!("{codec} · {bits}-bit / {} kHz{kbps}", khz(rate))
                    }
                    None => codec.clone(),
                };
                let native = fmt.is_some_and(|(rate, _)| n.graph_rate == rate);
                // Reported by the engine for this playback: what's actually audible.
                let level = n.level.get(&n.idx).copied().unwrap_or(1.0);
                let eq_on = self.eq_gains().iter().any(|&g| g != 0.0);
                let bitperfect = native && self.volume >= 1.0 && level == 1.0 && !eq_on && tx.is_none();
                let output = match fmt {
                    Some((rate, _)) if n.graph_rate == rate => format!("PipeWire · {} kHz matched", khz(rate)),
                    Some((rate, _)) if n.graph_rate > 0 => format!("PipeWire resamples {} → {} kHz", khz(rate), khz(n.graph_rate)),
                    _ => "PipeWire".into(),
                };
                let gain = if level != 1.0 {
                    format!("{} · {:+.1} dB applied (changes the samples)", self.leveling.label(), 20.0 * level.log10())
                } else if self.leveling == crate::theme::Leveling::Off {
                    "Off · samples untouched".into()
                } else {
                    format!("{} · 0 dB here (not analyzed, or already level)", self.leveling.label())
                };
                let steps: Vec<(&str, String, bool)> = vec![
                    ("Source", source, true),
                    match tx {
                        Some(k) => ("Plex stream", format!("Transcoded · MP3 ~{k} kbps · remote connection"), false),
                        None => ("Plex stream", "Direct play · original file · no transcode".into(), true),
                    },
                    ("Decoder", "symphonia · lock-free ring buffer".into(), true),
                    ("Loudness leveling", gain, level == 1.0),
                    (
                        "EQ",
                        if eq_on { format!("{} · changes the samples (preamp against clipping)", self.eq_preset.label()) } else { "Bypassed · samples untouched".into() },
                        !eq_on,
                    ),
                    (
                        "Volume",
                        if self.volume >= 1.0 { "100% · stream samples untouched".into() } else { format!("{:.0}% · PipeWire software volume (not bit-perfect)", self.volume * 100.0) },
                        self.volume >= 1.0,
                    ),
                    ("Output", output, native),
                ];
                div()
                    .flex()
                    .flex_col()
                    .children(steps.into_iter().map(|(k, v, ok)| {
                        div()
                            .flex()
                            .gap(px(12.0))
                            .child(
                                div()
                                    .w(px(14.0))
                                    .flex()
                                    .flex_col()
                                    .items_center()
                                    .child(div().mt(px(4.0)).size(px(10.0)).rounded(px(5.0)).bg(if ok { rgb(t.healthy).into() } else { white(0.4) }))
                                    .child(div().flex_1().w(px(2.0)).bg(white(0.14))),
                            )
                            .child(
                                div()
                                    .pb(px(16.0))
                                    .flex()
                                    .flex_col()
                                    .gap(px(3.0))
                                    .child(div().text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).text_color(white(1.0)).child(k))
                                    .child(div().font_family(MONO).text_size(px(11.0)).text_color(white(0.72)).child(v)),
                            )
                    }))
                    .child(
                        div()
                            .font_family(MONO)
                            .font_weight(FontWeight::MEDIUM)
                            .text_size(px(12.0))
                            .text_color(if bitperfect { rgb(t.healthy).into() } else { white(0.72) })
                            .child(if bitperfect {
                                "● bit-perfect from Crate: native rate, stream volume 100%"
                            } else if tx.is_some() {
                                "● transcoded by Plex for the remote connection"
                            } else if native && eq_on {
                                "● native rate · the EQ changes the samples"
                            } else if native && level != 1.0 {
                                "● native rate · loudness leveling changes the samples"
                            } else if native {
                                "● native rate · volume below 100% changes the samples"
                            } else {
                                "● resampled by PipeWire to the graph rate"
                            }),
                    )
            }
            _ => {
                let row = |k: &'static str, v: String, mono: bool| {
                    div()
                        .flex()
                        .gap(px(12.0))
                        .text_size(px(13.0))
                        .child(div().w(px(96.0)).flex_none().text_color(white(0.66)).child(k))
                        .child(div().flex_1().min_w_0().text_color(white(1.0)).when(mono, |d| d.font_family(MONO).text_size(px(12.0))).child(v))
                };
                match detail.as_ref() {
                    None => div().text_size(px(13.0)).text_color(white(0.6)).child("Loading…"),
                    Some(d) => div()
                        .flex()
                        .flex_col()
                        .gap(px(10.0))
                        .child(row(
                            "Plays",
                            if d.plays > 0 { format!("{} · last on {}", d.plays, day(d.last_viewed)) } else { "Not played yet".into() },
                            false,
                        ))
                        .child(row("Added", day(d.added), false))
                        .when(!d.moods.is_empty(), |el| el.child(row("Mood", d.moods.iter().take(3).cloned().collect::<Vec<_>>().join(", "), false)))
                        .when_some(d.loudness, |el, l| {
                            let g = d.gain.map(|g| format!(" · gain {g:+.1} dB")).unwrap_or_default();
                            el.child(row("Loudness", format!("{l:.1} LUFS{g}"), false))
                        })
                        .child(row("File", d.file.clone(), true)),
                }
            }
        };
        let panel = div()
            .w(px(360.0))
            .flex_none()
            .p(px(16.0))
            .rounded(px(16.0))
            .bg(gpui::hsla(240.0 / 360.0, 0.25, 0.06, 0.45))
            .border_1()
            .border_color(white(0.08))
            .flex()
            .flex_col()
            .gap(px(14.0))
            .child(tabs)
            .child(div().id("np-panel").flex_1().min_h_0().overflow_y_scroll().child(panel_body));

        div()
            .size_full()
            .relative()
            .bg(linear_gradient(200.0, linear_color_stop(rgb(c1), 0.0), linear_color_stop(rgb(mix(t.base, 0x000000, 0.3)), 1.0)))
            .child(div().absolute().inset_0().bg(linear_gradient(30.0, linear_color_stop(rgba(c2, 0.6), 0.0), linear_color_stop(rgba(c2, 0.0), 0.65))))
            .child(div().absolute().inset_0().bg(gpui::hsla(240.0 / 360.0, 0.2, 0.05, 0.28)))
            .child(
                div()
                    .relative()
                    .size_full()
                    .pt(px(20.0))
                    .px(px(32.0))
                    .pb(px(28.0))
                    .flex()
                    .flex_col()
                    .gap(px(20.0))
                    .child(top)
                    .child(div().flex_1().min_h_0().flex().gap(px(40.0)).child(left).child(lyric_col).child(panel)),
            )
            .into_any_element()
    }
}
