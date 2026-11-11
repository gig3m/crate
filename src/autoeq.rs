//! AutoEQ headphone presets (canvas: "AutoEQ · Sennheiser HD 600"). The
//! AutoEq project publishes, per headphone and measurement source, a
//! FixedBandEQ profile: ten peaking filters at exactly our bands (31 Hz ..
//! 16 kHz, Q 1.41). The index (~8,800 entries) is fetched once and cached
//! for 30 days; a profile is fetched when picked. Only GitHub is contacted.

use gpui::{AppContext, Context, Entity, Window};
use gpui_component::input::{InputEvent, InputState};

use crate::Crate;

const RAW: &str = "https://raw.githubusercontent.com/jaakkopasanen/AutoEq/master/results";
/// Measurement sources AutoEq recommends first, best first.
const SOURCES: [&str; 5] = ["oratory1990", "crinacle", "Rtings", "Innerfidelity", "Super Review"];

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Entry {
    pub name: String,
    pub source: String,
    /// Results path, URL-encoded, without the leading "./".
    pub path: String,
}

/// Parse INDEX.md lines: `- [Name](./source/kind/Name%20X) by source on rig`.
pub(crate) fn parse_index(md: &str) -> Vec<Entry> {
    md.lines()
        .filter_map(|l| {
            let l = l.strip_prefix("- [")?;
            let (name, rest) = l.split_once("](./")?;
            // Names and paths can hold parentheses: the link ends at ") by ".
            let (path, by) = rest.rsplit_once(") by ")?;
            // "oratory1990" or "crinacle on GRAS 43AG-7": the rig tells
            // apart measurements from the same source.
            let source = by.trim().to_string();
            Some(Entry { name: name.to_string(), source, path: path.to_string() })
        })
        .collect()
}

/// Ten band gains from a FixedBandEQ.txt, in band order; None unless all
/// ten bands are there at our frequencies.
pub(crate) fn parse_fixed_band(txt: &str) -> Option<[f32; 10]> {
    let mut gains = [f32::NAN; 10];
    for l in txt.lines() {
        let w: Vec<&str> = l.split_whitespace().collect();
        let fc = w.iter().position(|x| *x == "Fc").and_then(|i| w.get(i + 1)).and_then(|v| v.parse::<f32>().ok());
        let g = w.iter().position(|x| *x == "Gain").and_then(|i| w.get(i + 1)).and_then(|v| v.parse::<f32>().ok());
        if let (Some(fc), Some(g)) = (fc, g)
            // Within 5% of a band (62 vs 63 Hz, 16 vs 16.1 kHz spellings).
            && let Some(i) = crate_audio::audio::eq::BANDS.iter().position(|b| (b - fc).abs() <= b * 0.05)
        {
            gains[i] = g.clamp(-12.0, 12.0);
        }
    }
    gains.iter().all(|g| g.is_finite()).then_some(gains)
}

/// Best matches for `q`: every word must appear in the name; whole-name
/// and prefix matches first, then by recommended source.
pub(crate) fn search(index: &[Entry], q: &str, limit: usize) -> Vec<Entry> {
    let q = q.trim().to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    let words: Vec<&str> = q.split_whitespace().collect();
    let mut hits: Vec<(u8, usize, &Entry)> = index
        .iter()
        .filter_map(|e| {
            let n = e.name.to_lowercase();
            if !words.iter().all(|w| n.contains(w)) {
                return None;
            }
            let rank = if n == q { 0 } else if n.starts_with(&q) { 1 } else { 2 };
            let base = e.source.split(" on ").next().unwrap_or_default();
            let src = SOURCES.iter().position(|s| *s == base).unwrap_or(SOURCES.len());
            Some((rank, src, e))
        })
        .collect();
    hits.sort_by_key(|h| (h.0, h.2.name.len(), h.1));
    hits.into_iter().take(limit).map(|h| h.2.clone()).collect()
}

fn index_cache() -> std::path::PathBuf {
    crate::plex::cache_dir().join("autoeq-index.md")
}

async fn fetch_text(url: String) -> anyhow::Result<String> {
    let res = reqwest::Client::new().get(&url).timeout(std::time::Duration::from_secs(30)).send().await?;
    anyhow::ensure!(res.status().is_success(), "{} for {url}", res.status());
    Ok(res.text().await?)
}

/// The index: the cached copy when under 30 days old, else fetched.
async fn load_index() -> anyhow::Result<Vec<Entry>> {
    let path = index_cache();
    let fresh = std::fs::metadata(&path).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age.as_secs() < 30 * 86_400);
    if fresh && let Ok(md) = std::fs::read_to_string(&path) {
        let v = parse_index(&md);
        if !v.is_empty() {
            return Ok(v);
        }
    }
    let fetched = async {
        let md = fetch_text(format!("{RAW}/INDEX.md")).await?;
        let v = parse_index(&md);
        anyhow::ensure!(!v.is_empty(), "the AutoEq index came back empty");
        anyhow::Ok((md, v))
    }
    .await;
    match fetched {
        Ok((md, v)) => {
            // Atomic: a crash mid-write never leaves a truncated index.
            let _ = std::fs::create_dir_all(path.parent().unwrap_or(std::path::Path::new(".")));
            let tmp = path.with_extension("md.tmp");
            if std::fs::write(&tmp, md).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
            Ok(v)
        }
        // GitHub unreachable: an old list beats none.
        Err(e) => match std::fs::read_to_string(&path).map(|md| parse_index(&md)) {
            Ok(v) if !v.is_empty() => Ok(v),
            _ => Err(e),
        },
    }
}

/// Run a non-Plex request on the tokio side.
fn io_call<T: Send + 'static>(fut: impl std::future::Future<Output = anyhow::Result<T>> + Send + 'static) -> tokio::sync::oneshot::Receiver<anyhow::Result<T>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    match crate::PLEX.get() {
        Some((handle, _)) => {
            handle.spawn(async move {
                let _ = tx.send(fut.await);
            });
        }
        None => {
            let _ = tx.send(Err(anyhow::anyhow!("no runtime")));
        }
    }
    rx
}

/// The picker's state while open.
pub(crate) struct Picker {
    pub input: Entity<InputState>,
    pub results: Vec<Entry>,
    /// Still fetching the index.
    pub loading: bool,
}

impl Crate {
    /// Open (or close) the AutoEQ search under the presets.
    pub(crate) fn toggle_autoeq_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.autoeq_picker.take().is_some() {
            self.autoeq_pick_gen += 1;
            self.refocus = true;
            cx.notify();
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search headphones (e.g. HD 600, Sundara, AirPods Pro)"));
        cx.subscribe(&input, |this, input, ev: &InputEvent, cx| {
            if let InputEvent::Change = ev {
                let q = input.read(cx).value().to_string();
                if let (Some(p), Some(idx)) = (this.autoeq_picker.as_mut(), this.autoeq_index.as_ref()) {
                    p.results = search(idx, &q, 8);
                }
                cx.notify();
            }
        })
        .detach();
        input.update(cx, |s, cx| s.focus(window, cx));
        self.autoeq_picker = Some(Picker { input, results: Vec::new(), loading: self.autoeq_index.is_none() });
        if self.autoeq_index.is_none() {
            let rx = io_call(load_index());
            cx.spawn(async move |this, cx| {
                let res = rx.await;
                this.update(cx, |this, cx| {
                    match res {
                        Ok(Ok(v)) => {
                            this.autoeq_index = Some(std::sync::Arc::new(v));
                            if let Some(p) = this.autoeq_picker.as_mut() {
                                p.loading = false;
                                let q = p.input.read(cx).value().to_string();
                                if let Some(idx) = this.autoeq_index.as_ref() {
                                    p.results = search(idx, &q, 8);
                                }
                            }
                        }
                        Ok(Err(e)) => {
                            if let Some(p) = this.autoeq_picker.as_mut() {
                                p.loading = false;
                            }
                            this.say(format!("Couldn't load the AutoEQ list: {e:#}"));
                        }
                        Err(_) => {}
                    }
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        cx.notify();
    }

    /// Fetch a profile and make it the active preset.
    pub(crate) fn pick_autoeq(&mut self, e: Entry, window: &mut Window, cx: &mut Context<Self>) {
        let last = e.path.rsplit('/').next().unwrap_or_default().to_string();
        let url = format!("{RAW}/{}/{last}%20FixedBandEQ.txt", e.path);
        let rx = io_call(fetch_text(url));
        let label = format!("{} · {}", e.name, e.source);
        self.say(format!("Loading AutoEQ for {label}…"));
        // The last row clicked wins, whatever order replies arrive in.
        self.autoeq_pick_gen += 1;
        let gen_ = self.autoeq_pick_gen;
        let window_handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let res = rx.await;
            cx.update_window(window_handle, |_, window, cx| {
                this.update(cx, |this, cx| {
                    if this.autoeq_pick_gen != gen_ {
                        return;
                    }
                    match res {
                        Ok(Ok(txt)) => match parse_fixed_band(&txt) {
                            Some(g) => {
                                this.eq_autoeq = Some((e.name.clone(), g));
                                if let Err(err) = crate::theme::save_autoeq(&e.name, &g) {
                                    this.status = format!("Couldn't save the AutoEQ preset: {err}").into();
                                }
                                this.autoeq_picker = None;
                                this.refocus = true;
                                this.set_eq_preset(crate::theme::EqPreset::AutoEq, window, cx);
                                this.say(format!("AutoEQ: {label}"));
                            }
                            None => this.say(format!("That AutoEQ profile isn't ten-band ({label})")),
                        },
                        Ok(Err(err)) => this.say(format!("Couldn't load AutoEQ for {label}: {err:#}")),
                        Err(_) => {}
                    }
                    cx.notify();
                })
                .ok();
            })
            .ok();
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_index_and_profile() {
        let md = "# Index\n\n- [Sennheiser HD 600](./oratory1990/over-ear/Sennheiser%20HD%20600) by oratory1990\n- [Sennheiser HD 600](./crinacle/GRAS%2043AG-7%20over-ear/Sennheiser%20HD%20600) by crinacle on GRAS 43AG-7\n- [Sennheiser HD 6XX](./oratory1990/over-ear/Sennheiser%20HD%206XX) by oratory1990\n";
        let idx = parse_index(md);
        assert_eq!(idx.len(), 3);
        let paren = parse_index("- [1MORE Aero (ANC Off)](./HypetheSonics/GRAS%20RA0045%20in-ear/1MORE%20Aero%20(ANC%20Off)) by HypetheSonics on GRAS RA0045");
        assert_eq!(paren.len(), 1);
        assert_eq!(paren[0].path, "HypetheSonics/GRAS%20RA0045%20in-ear/1MORE%20Aero%20(ANC%20Off)");
        assert_eq!(paren[0].name, "1MORE Aero (ANC Off)");
        assert_eq!(idx[1].source, "crinacle on GRAS 43AG-7");
        assert_eq!(idx[0].path, "oratory1990/over-ear/Sennheiser%20HD%20600");
        let hits = search(&idx, "hd 600", 8);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].source, "oratory1990");
        let txt = "Preamp: -7.5 dB\nFilter 1: ON PK Fc 31 Hz Gain 6.9 dB Q 1.41\nFilter 2: ON PK Fc 62 Hz Gain 3.3 dB Q 1.41\nFilter 3: ON PK Fc 125 Hz Gain -1.1 dB Q 1.41\nFilter 4: ON PK Fc 250 Hz Gain -1.6 dB Q 1.41\nFilter 5: ON PK Fc 500 Hz Gain 0.6 dB Q 1.41\nFilter 6: ON PK Fc 1000 Hz Gain -0.8 dB Q 1.41\nFilter 7: ON PK Fc 2000 Hz Gain 0.1 dB Q 1.41\nFilter 8: ON PK Fc 4000 Hz Gain -1.0 dB Q 1.41\nFilter 9: ON PK Fc 8000 Hz Gain 3.9 dB Q 1.41\nFilter 10: ON PK Fc 16000 Hz Gain -6.5 dB Q 1.41\n";
        let g = parse_fixed_band(txt).unwrap();
        assert_eq!(g[0], 6.9);
        assert_eq!(g[9], -6.5);
        assert!(parse_fixed_band("Filter 1: ON PK Fc 31 Hz Gain 1 dB Q 1.41").is_none());
    }
}
