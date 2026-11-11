//! Audio settings from the design canvas (AudioSettings.dc.html), limited to
//! what the engine does today: output device choice and the bit-perfect
//! status of the PipeWire graph. Leveling, crossfade, EQ and transcoding
//! need engine work first.

use gpui::{Context, FontWeight, div, prelude::*, px, rgb};

use crate::{Aulos, PAD_X, shell::MONO, theme::Theme};

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
/// the file's sample rate permanently. Aulos itself never edits system config.
pub(crate) fn pipewire_prompt(sink: &str, allowed: &str) -> String {
    format!(
        r#"Set up PipeWire on this Linux machine so the audio graph can run at the playing file's sample rate (bit-perfect playback for the Aulos music player) instead of resampling everything to one rate.

Current state: clock.allowed-rates is {allowed}. Aulos plays to the sink "{sink}".

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

impl Aulos {
    pub(crate) fn open_audio_settings(&mut self, cx: &mut Context<Self>) {
        self.go(crate::View::Soon("audio"));
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
                "PipeWire runs every app at one shared rate, so files at other rates are resampled. To play bit-perfect, PipeWire has to be allowed to switch to your device's other rates (clock.allowed-rates). That's a system setting, so Aulos doesn't change it; the prompt below walks your coding agent through it. Trade-off: while Aulos plays, other apps share its rate, and the device briefly re-clocks when the rate changes."
            } else {
                "PipeWire is allowed to follow the file's rate. Aulos asks for each file's own rate; a device that can't do a rate gets the nearest one it supports, resampled."
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

        let later = card()
            .child(div().text_size(px(14.0)).font_weight(FontWeight::MEDIUM).text_color(rgb(t.text_strong)).child("Loudness leveling, sweet fades, EQ"))
            .child(div().text_size(px(12.0)).text_color(rgb(t.muted)).child(
                "Not in the engine yet. Each one changes the samples, so they'll be off by default and the signal path will say when they're on.",
            ));

        div()
            .id("audio-settings")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px(px(PAD_X))
            .pt(px(4.0))
            .pb(px(40.0))
            .flex()
            .flex_col()
            .gap(px(26.0))
            .child(div().text_size(px(28.0)).font_weight(FontWeight::BOLD).text_color(rgb(t.text_strong)).child("Audio"))
            .child(
                div()
                    .flex()
                    .gap(px(24.0))
                    .child(div().flex_1().min_w_0().flex().flex_col().gap(px(12.0)).child(h2("Output device")).children(devices))
                    .child(div().flex_1().min_w_0().flex().flex_col().gap(px(12.0)).child(h2("Signal")).child(bitperfect).child(later)),
            )
            .when(!current.is_empty(), |d| {
                d.child(div().font_family(MONO).text_size(px(11.0)).text_color(rgb(t.muted)).child(format!("Playing to {current}")))
            })
            .into_any_element()
    }
}
