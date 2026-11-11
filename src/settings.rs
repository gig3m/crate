//! Audio settings from the design canvas (AudioSettings.dc.html), limited to
//! what the engine does today: output device choice and the bit-perfect
//! status of the PipeWire graph, leveling, crossfade, remote streaming
//! quality and the EQ.

use gpui::{Context, FontWeight, div, prelude::*, px, rgb};

use crate::{Crate, PAD_X, shell::MONO, theme::Theme};

#[derive(Clone, Debug, Default)]
pub(crate) struct AudioInfo {
    /// (node.name, description)
    pub sinks: Vec<(String, String)>,
    pub default_sink: String,
    pub allowed_rates: String,
    pub clock_rate: String,
}

/// Read sinks and clock settings from `pw-dump` (runs off the UI thread).
fn read_audio_info() -> anyhow::Result<AudioInfo> {
    let out = std::process::Command::new("pw-dump").output()?;
    anyhow::ensure!(out.status.success(), "pw-dump failed");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let mut info = AudioInfo::default();
    for o in v.as_array().into_iter().flatten() {
        let props = &o["info"]["props"];
        if props["media.class"].as_str() == Some("Audio/Sink")
            && let Some(name) = props["node.name"].as_str()
        {
            let desc = props["node.description"].as_str().unwrap_or(name).to_string();
            info.sinks.push((name.to_string(), desc));
        }
        if o["type"].as_str() == Some("PipeWire:Interface:Metadata") {
            let which = o["props"]["metadata.name"].as_str().unwrap_or_default();
            for m in o["metadata"].as_array().into_iter().flatten() {
                let (k, val) = (m["key"].as_str().unwrap_or_default(), &m["value"]);
                match (which, k) {
                    ("default", "default.audio.sink") => info.default_sink = val["name"].as_str().unwrap_or_default().to_string(),
                    ("settings", "clock.allowed-rates") => info.allowed_rates = val.as_str().map(String::from).unwrap_or_else(|| val.to_string()),
                    ("settings", "clock.rate") => info.clock_rate = val.as_str().map(String::from).unwrap_or_else(|| val.to_string()),
                    _ => {}
                }
            }
        }
    }
    Ok(info)
}

/// A prompt the user can hand to their coding agent to make PipeWire follow
/// the file's sample rate permanently. Crate itself never edits system config.
pub(crate) fn pipewire_prompt(sink: &str, allowed: &str) -> String {
    format!(
        r#"Set up PipeWire on this Linux machine so the audio graph can run at the playing file's sample rate (bit-perfect playback for the Crate music player) instead of resampling everything to one rate.

Current state: clock.allowed-rates is {allowed}. Crate plays to the sink "{sink}".

Please:
1. Find the playback sample rates each of my output devices supports. For USB/ALSA devices, read the "Rates:" lines under "Playback" in /proc/asound/card*/stream*; for others, use `pw-dump` (the node's EnumFormat params) or `pactl list sinks`.
2. Create ~/.config/pipewire/pipewire.conf.d/10-allowed-rates.conf with
   context.properties = {{ default.clock.allowed-rates = [ ... ] }}
   listing the union of those rates among 44100 48000 88200 96000 176400 192000, with a comment naming which device supports which. If I manage ~/.config with a dotfiles repo, put the file there and link it the way that repo does.
3. Check it parses: `pw-config merge context.properties` should show the new list.
4. Apply it without restarting audio: `pw-metadata -n settings 0 clock.allowed-rates "[ ... ]"`. Don't restart PipeWire unless I say so (it cuts off whatever is playing).
5. Confirm with `pw-metadata -n settings 0 clock.allowed-rates`, and tell me how to undo it (delete the file, restart PipeWire or set the metadata back to the old value).

Trade-off to mention to me: while a file plays at, say, 44.1 kHz, other apps that start at 48 kHz are resampled to the playing rate, and the device briefly re-clocks when the rate changes between tracks."#
    )
}

impl Crate {
    /// Ten-band EQ (canvas): preset chips over one vertical slider per band.
    fn eq_card(&self, t: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        use crate::theme::EqPreset;
        let cur = self.eq_preset;
        let chips = EqPreset::ALL.into_iter().map(|p| {
            let on = p == cur;
            div()
                .id(("eq-preset", p as usize))
                .h(px(28.0))
                .px(px(12.0))
                .flex()
                .items_center()
                .rounded(px(14.0))
                .border_1()
                .border_color(rgb(if on { t.accent } else { t.raised }))
                .when(on, |d| d.bg(rgb(t.raised)))
                .text_size(px(12.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(rgb(if on { t.text_strong } else { t.text2 }))
                .cursor_pointer()
                .child(match (p, self.eq_autoeq.as_ref()) {
                    (EqPreset::AutoEq, Some((name, _))) => format!("AutoEQ · {name}"),
                    (EqPreset::AutoEq, None) => "AutoEQ…".to_string(),
                    _ => p.label().to_string(),
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    // AutoEQ: pick a headphone first, or again when it's already on.
                    if p == EqPreset::AutoEq && (this.eq_autoeq.is_none() || this.eq_preset == EqPreset::AutoEq) {
                        this.toggle_autoeq_picker(window, cx);
                    } else {
                        this.set_eq_preset(p, window, cx);
                    }
                }))
        });
        let picker = self.autoeq_picker.as_ref().map(|pk| {
            let rows = pk.results.iter().enumerate().map(|(i, e)| {
                let e2 = e.clone();
                div()
                    .id(("autoeq-hit", i))
                    .px(px(10.0))
                    .py(px(6.0))
                    .rounded(px(6.0))
                    .flex()
                    .justify_between()
                    .gap(px(8.0))
                    .cursor_pointer()
                    .hover(|d| d.bg(rgb(t.raised)))
                    .child(div().text_size(px(13.0)).text_color(rgb(t.text)).truncate().child(e.name.clone()))
                    .child(div().flex_none().text_size(px(11.0)).text_color(rgb(t.muted)).child(e.source.clone()))
                    .on_click(cx.listener(move |this, _, window, cx| this.pick_autoeq(e2.clone(), window, cx)))
            });
            let note = if pk.loading {
                Some("Loading the AutoEq list…")
            } else if pk.results.is_empty() && !pk.input.read(cx).value().trim().is_empty() {
                Some("No headphone by that name in AutoEq")
            } else {
                None
            };
            div()
                .flex()
                .flex_col()
                .gap(px(4.0))
                .p(px(8.0))
                .rounded(px(10.0))
                .border_1()
                .border_color(rgb(t.raised))
                .bg(rgb(t.base))
                .child(
                    div()
                        .h(px(32.0))
                        .px(px(4.0))
                        .rounded(px(8.0))
                        .border_1()
                        .border_color(rgb(t.border))
                        .bg(rgb(t.surface))
                        .child(gpui_component::input::Input::new(&pk.input).appearance(false)),
                )
                .children(note.map(|n| div().px(px(10.0)).py(px(4.0)).text_size(px(12.0)).text_color(rgb(t.muted)).child(n)))
                .children(rows)
                .child(div().px(px(10.0)).text_size(px(11.0)).text_color(rgb(t.muted)).child("Corrections from the AutoEq project (github.com/jaakkopasanen/AutoEq), ten bands at Q 1.41."))
        });
        let gains = self.eq_gains();
        let shown = self.preset_curve(cur).unwrap_or(self.eq_custom);
        let bands = self.eq_sliders.iter().enumerate().map(|(i, s)| {
            let f = crate_audio::audio::eq::BANDS[i];
            let label = if f >= 1000.0 { format!("{}k", f / 1000.0) } else { format!("{f}") };
            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(6.0))
                .child(div().font_family(crate::shell::MONO).text_size(px(10.0)).text_color(rgb(t.text2)).child(format!("{:+.1}", shown[i])))
                .child(gpui_component::slider::Slider::new(s).vertical().disabled(cur == EqPreset::Off))
                .child(div().font_family(crate::shell::MONO).text_size(px(10.0)).text_color(rgb(t.muted)).child(label))
        });
        let active = gains.iter().any(|&g| g != 0.0);
        div()
            .p(px(16.0))
            .rounded(px(12.0))
            .border_1()
            .border_color(rgb(t.raised))
            .bg(rgb(t.surface))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(div().text_size(px(14.0)).font_weight(FontWeight::MEDIUM).text_color(rgb(t.text_strong)).child("Ten-band equalizer"))
            .child(div().flex().flex_wrap().gap(px(6.0)).children(chips))
            .children(picker)
            .child(div().pt(px(6.0)).flex().gap(px(4.0)).opacity(if cur == EqPreset::Off { 0.45 } else { 1.0 }).children(bands))
            .child(div().text_size(px(12.0)).text_color(rgb(t.muted)).child(if active {
                "Heard right away. Changes the samples. A preamp takes off the biggest boost and a soft knee rounds any peak past 0.98 instead of clipping it."
            } else {
                "Off: bypassed, samples untouched (bit-perfect). Moving a slider switches to Custom."
            }))
            .into_any_element()
    }

    /// Theme: Crate's own palette, or follow Omarchy.
    fn theme_section(&self, t: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        use crate::theme::{Mode, Theme as Th};
        let option = |id: &'static str, mode: Mode, title: &'static str, blurb: &'static str, preview: Th, cx: &mut Context<Self>| {
            let on = self.theme_mode == mode;
            let dots = [preview.surface, preview.raised, preview.text_strong, preview.accent, preview.discovery, preview.healthy, preview.yellow, preview.red];
            div()
                .id(id)
                .flex_1()
                .min_w_0()
                .p(px(16.0))
                .rounded(px(12.0))
                .border_1()
                .border_color(rgb(if on { t.accent } else { t.border }))
                .bg(rgb(if on { t.raised } else { t.surface }))
                .flex()
                .flex_col()
                .gap(px(10.0))
                .cursor_pointer()
                .hover(|d| d.bg(rgb(t.raised)))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .child(
                            div()
                                .size(px(18.0))
                                .flex_none()
                                .rounded(px(9.0))
                                .border_2()
                                .border_color(rgb(if on { t.accent } else { t.line }))
                                .flex()
                                .items_center()
                                .justify_center()
                                .when(on, |d| d.child(div().size(px(8.0)).rounded(px(4.0)).bg(rgb(t.accent)))),
                        )
                        .child(div().text_size(px(15.0)).font_weight(FontWeight::SEMIBOLD).text_color(rgb(t.text_strong)).child(title)),
                )
                .child(div().text_size(px(12.0)).text_color(rgb(t.muted)).child(blurb))
                .child(div().flex().gap(px(6.0)).children(dots.into_iter().map(|c| div().size(px(18.0)).rounded(px(5.0)).bg(rgb(c)).border_1().border_color(rgb(t.line)))))
                .on_click(cx.listener(move |this, _, _, cx| this.set_theme_mode(mode, cx)))
        };
        div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(div().text_size(px(20.0)).font_weight(FontWeight::SEMIBOLD).text_color(rgb(t.text_strong)).child("Theme"))
            .child(
                div()
                    .flex()
                    .gap(px(16.0))
                    .child(option(
                        "theme-crate",
                        Mode::Crate,
                        "Crate",
                        "The app's own palette from its design: deep blue-grey layers, white titles, blue, purple and green.",
                        self.theme_previews.as_ref().map(|p| p.0.clone()).unwrap_or_else(Th::crate_default),
                        cx,
                    ))
                    .child(option(
                        "theme-omarchy",
                        Mode::Omarchy,
                        "Follow Omarchy",
                        "Colors from your current Omarchy theme, updating live when you switch themes.",
                        self.theme_previews.as_ref().map(|p| p.1.clone()).unwrap_or_else(Th::crate_default),
                        cx,
                    )),
            )
            .into_any_element()
    }

    pub(crate) fn open_audio_settings(&mut self, cx: &mut Context<Self>) {
        self.go(crate::View::Soon("audio"));
        use crate::theme::{Mode, Theme as Th};
        self.theme_previews = Some((Th::load(Mode::Crate).0, Th::load(Mode::Omarchy).0));
        let task = cx.background_spawn(async { read_audio_info() });
        cx.spawn(async move |this, cx| {
            let res = task.await;
            this.update(cx, |this, cx| {
                match res {
                    Ok(i) => this.audio_info = Some(i),
                    Err(e) => this.status = format!("PipeWire: {e:#}").into(),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Switch output; restarts playback at the same position on the new sink.
    fn set_output(&mut self, name: Option<String>, cx: &mut Context<Self>) {
        self.target = name;
        if let Some(n) = self.now.as_ref()
            && !n.finished
            && self.engine.is_some()
        {
            let (idx, ms, paused) = (n.idx, n.elapsed_ms, n.paused);
            self.play_from(idx, ms, cx);
            if paused {
                self.transport(crate::mpris::Transport::Pause, cx);
            }
        }
        cx.notify();
    }

    pub(crate) fn audio_settings_view(&self, t: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let h2 = |s: &'static str| div().text_size(px(15.0)).font_weight(FontWeight::SEMIBOLD).text_color(rgb(t.text_strong)).child(s);
        let card = || div().p(px(16.0)).rounded(px(12.0)).border_1().border_color(rgb(t.raised)).bg(rgb(t.surface)).flex().flex_col().gap(px(8.0));
        let Some(info) = self.audio_info.clone() else {
            return div().px(px(PAD_X)).text_color(rgb(t.muted)).child("Reading PipeWire…").into_any_element();
        };
        let current = self.target.clone().unwrap_or_else(|| info.default_sink.clone());
        let mut rows: Vec<(Option<String>, String, String)> = vec![(None, "System default".into(), info.default_sink.clone())];
        rows.extend(info.sinks.iter().map(|(n, d)| (Some(n.clone()), d.clone(), n.clone())));
        let devices: Vec<_> = rows
            .into_iter()
            .enumerate()
            .map(|(i, (target, title, sub))| {
                let on = match &target {
                    None => self.target.is_none(),
                    Some(n) => self.target.as_deref() == Some(n.as_str()),
                };
                div()
                    .id(("dev", i))
                    .px(px(14.0))
                    .py(px(12.0))
                    .flex()
                    .items_center()
                    .gap(px(14.0))
                    .rounded(px(12.0))
                    .border_1()
                    .border_color(rgb(if on { t.accent } else { t.border }))
                    .bg(rgb(if on { t.raised } else { t.surface }))
                    .cursor_pointer()
                    .hover(|d| d.bg(rgb(t.raised)))
                    .child(
                        div()
                            .size(px(18.0))
                            .flex_none()
                            .rounded(px(9.0))
                            .border_2()
                            .border_color(rgb(if on { t.accent } else { t.line }))
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(on, |d| d.child(div().size(px(8.0)).rounded(px(4.0)).bg(rgb(t.accent)))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .child(div().text_size(px(14.0)).font_weight(FontWeight::SEMIBOLD).text_color(rgb(t.text_strong)).truncate().child(title))
                            .child(div().font_family(MONO).text_size(px(11.0)).text_color(rgb(t.muted)).truncate().child(sub)),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.set_output(target.clone(), cx)))
            })
            .collect();

        // Bit-perfect status of the graph.
        let rates = info.allowed_rates.trim().to_string();
        let only_one = rates.split(|c: char| !c.is_ascii_digit()).filter(|w| !w.is_empty()).count() <= 1;
        let now_line = self.now.as_ref().and_then(|n| {
            let &(rate, _) = n.formats.get(&n.idx)?;
            Some(if n.graph_rate == rate {
                (format!("Now playing at {} Hz: native, no resampling.", rate), t.healthy)
            } else {
                (format!("Now playing a {} Hz file through a {} Hz graph: PipeWire resamples.", rate, n.graph_rate), t.text2)
            })
        });
        let bitperfect = card()
            .child(div().text_size(px(14.0)).font_weight(FontWeight::MEDIUM).text_color(rgb(t.text_strong)).child("Bit-perfect output"))
            .child(
                div()
                    .font_family(MONO)
                    .text_size(px(12.0))
                    .text_color(rgb(t.text2))
                    .child(format!("clock.allowed-rates {rates} · clock.rate {}", info.clock_rate)),
            )
            .child(div().text_size(px(12.0)).text_color(rgb(t.muted)).child(if only_one {
                "PipeWire runs every app at one shared rate, so files at other rates are resampled. To play bit-perfect, PipeWire has to be allowed to switch to your device's other rates (clock.allowed-rates). That's a system setting, so Crate doesn't change it; the prompt below walks your coding agent through it. Trade-off: while Crate plays, other apps share its rate, and the device briefly re-clocks when the rate changes."
            } else {
                "PipeWire is allowed to follow the file's rate. Crate asks for each file's own rate; a device that can't do a rate gets the nearest one it supports, resampled."
            }))
            .when_some(now_line, |d, (l, c)| d.child(div().text_size(px(12.0)).text_color(rgb(c)).child(l)))
            .when(only_one, |d| {
                let prompt = pipewire_prompt(&current, &rates);
                d.child(
                    div()
                        .mt(px(6.0))
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .child(
                            div()
                                .id("copy-pw-prompt")
                                .h(px(32.0))
                                .px(px(14.0))
                                .flex()
                                .items_center()
                                .rounded(px(16.0))
                                .bg(rgb(t.accent))
                                .text_color(rgb(t.base))
                                .text_size(px(13.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .cursor_pointer()
                                .hover(|d| d.opacity(0.9))
                                .child(if self.prompt_copied { "Copied" } else { "Copy setup prompt" })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(prompt.clone()));
                                    this.prompt_copied = true;
                                    cx.notify();
                                })),
                        )
                        .child(div().text_size(px(12.0)).text_color(rgb(t.muted)).child("Paste it into your coding agent (Claude Code, Codex…) to make the change.")),
                )
            });

        // Loudness leveling (canvas: Off / Track / Album / Smart).
        let cur = self.leveling;
        let segs = crate::theme::Leveling::ALL.into_iter().map(|l| {
            let on = l == cur;
            div()
                .id(("leveling", l as usize))
                .flex_1()
                .h(px(30.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(7.0))
                .text_size(px(13.0))
                .font_weight(FontWeight::MEDIUM)
                .cursor_pointer()
                .when(on, |d| d.bg(rgb(t.raised)).text_color(rgb(t.text_strong)))
                .when(!on, |d| d.text_color(rgb(t.text2)).hover(|d| d.text_color(rgb(t.text_strong))))
                .child(l.label())
                .on_click(cx.listener(move |this, _, _, cx| this.set_leveling(l, cx)))
        });
        let later = card()
            .child(div().text_size(px(14.0)).font_weight(FontWeight::MEDIUM).text_color(rgb(t.text_strong)).child("Loudness leveling"))
            .child(div().p(px(3.0)).rounded(px(9.0)).border_1().border_color(rgb(t.raised)).bg(rgb(t.base)).flex().gap(px(2.0)).children(segs))
            .child(div().text_size(px(12.0)).text_color(rgb(t.muted)).child(match cur {
                crate::theme::Leveling::Off => "Off: samples stay untouched (bit-perfect).",
                crate::theme::Leveling::Track => "Each track to the same loudness, from Plex's analysis. Changes the samples.",
                crate::theme::Leveling::Album => "Whole albums to the same loudness, keeping their own dynamics. Changes the samples.",
                crate::theme::Leveling::Smart => "Smart: album gain when you play an album in order, track gain on shuffle and radio. Changes the samples.",
            }))
            .child(div().text_size(px(12.0)).text_color(rgb(t.muted)).child("Applies from the next track. Never boosts past a track's peak."));
        let fade = self.fade_secs;
        let fade_segs = crate::theme::FADES.into_iter().map(|secs| {
            let on = secs == fade;
            div()
                .id(("fade", secs as usize))
                .flex_1()
                .h(px(30.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(7.0))
                .text_size(px(13.0))
                .font_weight(FontWeight::MEDIUM)
                .cursor_pointer()
                .when(on, |d| d.bg(rgb(t.raised)).text_color(rgb(t.text_strong)))
                .when(!on, |d| d.text_color(rgb(t.text2)).hover(|d| d.text_color(rgb(t.text_strong))))
                .child(if secs == 0 { "Off".to_string() } else { format!("{secs} s") })
                .on_click(cx.listener(move |this, _, _, cx| this.set_fade(secs, cx)))
        });
        let fades = card()
            .child(div().text_size(px(14.0)).font_weight(FontWeight::MEDIUM).text_color(rgb(t.text_strong)).child("Sweet fades"))
            .child(div().p(px(3.0)).rounded(px(9.0)).border_1().border_color(rgb(t.raised)).bg(rgb(t.base)).flex().gap(px(2.0)).children(fade_segs))
            .child(div().text_size(px(12.0)).text_color(rgb(t.muted)).child(
                "Crossfades only between tracks from different albums. Gapless inside albums is never broken. Needs both tracks at the same sample rate; otherwise they play back to back.",
            ));
        let kbps = self.remote_kbps;
        let remote = crate::REMOTE.get().copied().unwrap_or(false);
        let q_segs = crate::theme::REMOTE_QUALITIES.into_iter().map(|q| {
            let on = q == kbps;
            div()
                .id(("remote-quality", q as usize))
                .flex_1()
                .h(px(30.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(7.0))
                .text_size(px(13.0))
                .font_weight(FontWeight::MEDIUM)
                .cursor_pointer()
                .when(on, |d| d.bg(rgb(t.raised)).text_color(rgb(t.text_strong)))
                .when(!on, |d| d.text_color(rgb(t.text2)).hover(|d| d.text_color(rgb(t.text_strong))))
                .child(if q == 0 { "Original".to_string() } else { format!("{q}k") })
                .on_click(cx.listener(move |this, _, _, cx| this.set_remote_quality(q, cx)))
        });
        let quality = card()
            .child(
                div()
                    .flex()
                    .justify_between()
                    .child(div().text_size(px(14.0)).font_weight(FontWeight::MEDIUM).text_color(rgb(t.text_strong)).child("Remote streaming quality"))
                    .child(div().text_size(px(13.0)).text_color(rgb(t.text2)).child("Original on LAN")),
            )
            .child(div().p(px(3.0)).rounded(px(9.0)).border_1().border_color(rgb(t.raised)).bg(rgb(t.base)).flex().gap(px(2.0)).children(q_segs))
            .child(div().text_size(px(12.0)).text_color(rgb(t.muted)).child(match (remote, kbps) {
                (false, _) => "Connected on this network: every track plays as the original file.".to_string(),
                (true, 0) => "Connected remotely, playing original files.".to_string(),
                (true, k) => format!("Connected remotely: Plex converts tracks to MP3 at about {k} kbps (not bit-perfect). Applies from the next track."),
            }));

        div()
            .id("audio-settings")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.settings_scroll)
            .px(px(PAD_X))
            .pt(px(4.0))
            .pb(px(40.0))
            .flex()
            .flex_col()
            .gap(px(26.0))
            .child(div().text_size(px(28.0)).font_weight(FontWeight::BOLD).text_color(rgb(t.text_strong)).child("Settings"))
            .child(self.theme_section(t, cx))
            .child(div().text_size(px(20.0)).font_weight(FontWeight::SEMIBOLD).text_color(rgb(t.text_strong)).child("Audio"))
            .child(
                div()
                    .flex()
                    .gap(px(24.0))
                    .child(div().flex_1().min_w_0().flex().flex_col().gap(px(12.0)).child(h2("Output device")).children(devices))
                    .child(div().flex_1().min_w_0().flex().flex_col().gap(px(12.0)).child(h2("Signal")).child(bitperfect).child(later).child(fades).child(quality).child(self.eq_card(t, cx))),
            )
            .when(!current.is_empty(), |d| {
                d.child(div().font_family(MONO).text_size(px(11.0)).text_color(rgb(t.muted)).child(format!("Playing to {current}")))
            })
            .into_any_element()
    }
}
