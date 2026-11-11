//! Color roles. Two sources:
//! - **Crate** (default): the design canvas palette, fixed.
//! - **Follow Omarchy**: derived from the active Omarchy theme,
//!   ~/.local/state/omarchy/current/theme/colors.toml (Omarchy v4);
//!   `theme.generation` beside it changes on every theme switch.
//!
//! The choice lives in ~/.config/crate/config.toml (`theme = "crate" | "omarchy"`).

use std::{collections::HashMap, path::PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Crate,
    Omarchy,
}

#[derive(Clone, Debug)]
pub struct Theme {
    pub base: u32,
    pub surface: u32,
    pub raised: u32,
    pub border: u32,
    pub text: u32,
    pub text_strong: u32,
    /// Artists and metadata.
    pub text2: u32,
    /// Labels, counts, timestamps (≥ 4.5:1 on surface).
    pub muted: u32,
    pub accent: u32,
    /// Sonic features, similar.
    pub discovery: u32,
    /// Bit-perfect, server online.
    pub healthy: u32,
    /// Player bar: a step below base.
    pub deep: u32,
    /// Inner borders on raised items.
    pub line: u32,
    /// Palette for generated art (stations, placeholders) and swatches.
    pub red: u32,
    pub orange: u32,
    pub yellow: u32,
    pub green: u32,
    pub cyan: u32,
    pub teal: u32,
    pub blue: u32,
    pub magenta: u32,
}

fn state_dir() -> PathBuf {
    if let Ok(d) = std::env::var("CRATE_THEME_DIR") {
        return PathBuf::from(d);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".local/state/omarchy/current")
}

pub fn generation() -> String {
    std::fs::read_to_string(state_dir().join("theme.generation")).unwrap_or_default().trim().to_string()
}

fn config_path() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config"));
    base.join("crate/config.toml")
}

/// The saved theme choice (Crate when unset).
pub fn saved_mode() -> Mode {
    let raw = std::fs::read_to_string(config_path()).unwrap_or_default();
    let map: HashMap<String, toml::Value> = toml::from_str(&raw).unwrap_or_default();
    match map.get("theme").and_then(|v| v.as_str()) {
        Some("omarchy") => Mode::Omarchy,
        _ => Mode::Crate,
    }
}

/// Save the theme choice, keeping any other keys in the config file.
pub fn save_mode(mode: Mode) -> std::io::Result<()> {
    save_key("theme", toml::Value::String(if mode == Mode::Omarchy { "omarchy" } else { "crate" }.into()))
}

/// Saved volume (perceptual 0..=1), full when unset.
pub fn saved_volume() -> f32 {
    let raw = std::fs::read_to_string(config_path()).unwrap_or_default();
    let map: HashMap<String, toml::Value> = toml::from_str(&raw).unwrap_or_default();
    map.get("volume").and_then(|v| v.as_float()).filter(|v| v.is_finite()).map(|v| v.clamp(0.0, 1.0) as f32).unwrap_or(1.0)
}

/// Loudness leveling (canvas: Off / Track / Album / Smart). Off keeps
/// playback bit-perfect, so it's the default.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Leveling {
    #[default]
    Off,
    Track,
    Album,
    /// Album gain for an album played in order, track gain otherwise.
    Smart,
}

impl Leveling {
    pub const ALL: [Leveling; 4] = [Leveling::Off, Leveling::Track, Leveling::Album, Leveling::Smart];

    pub fn label(self) -> &'static str {
        match self {
            Leveling::Off => "Off",
            Leveling::Track => "Track",
            Leveling::Album => "Album",
            Leveling::Smart => "Smart",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Leveling::Off => "off",
            Leveling::Track => "track",
            Leveling::Album => "album",
            Leveling::Smart => "smart",
        }
    }
}

pub fn saved_leveling() -> Leveling {
    let raw = std::fs::read_to_string(config_path()).unwrap_or_default();
    let map: HashMap<String, toml::Value> = toml::from_str(&raw).unwrap_or_default();
    let v = map.get("leveling").and_then(|v| v.as_str()).unwrap_or("off");
    Leveling::ALL.into_iter().find(|l| l.key() == v).unwrap_or_default()
}

/// Sweet fades between albums, seconds: one of 0 (off), 3, 6, 10.
pub const FADES: [u32; 4] = [0, 3, 6, 10];

pub fn saved_fade() -> u32 {
    let raw = std::fs::read_to_string(config_path()).unwrap_or_default();
    let map: HashMap<String, toml::Value> = toml::from_str(&raw).unwrap_or_default();
    map.get("fade").and_then(|v| v.as_integer()).map(|v| v as u32).filter(|v| FADES.contains(v)).unwrap_or(0)
}

pub fn save_fade(secs: u32) -> std::io::Result<()> {
    save_key("fade", toml::Value::Integer(secs as i64))
}

/// Track bar visualizer: (style key, palette index).
pub fn saved_viz() -> (String, usize) {
    let raw = std::fs::read_to_string(config_path()).unwrap_or_default();
    let map: HashMap<String, toml::Value> = toml::from_str(&raw).unwrap_or_default();
    let style = map.get("viz").and_then(|v| v.as_str()).unwrap_or("sonic").to_string();
    let palette = map.get("viz_colors").and_then(|v| v.as_integer()).map(|v| v.max(0) as usize).unwrap_or(0);
    (style, palette)
}

pub fn save_viz(style: &str, palette: usize) -> std::io::Result<()> {
    save_key("viz", toml::Value::String(style.into()))?;
    save_key("viz_colors", toml::Value::Integer(palette as i64))
}

/// Remote streaming quality, kbps: 0 = original file. Applies only when the
/// server address in use isn't on this network (canvas: "Original on LAN").
pub const REMOTE_QUALITIES: [u32; 4] = [0, 320, 192, 128];

pub fn saved_remote_kbps() -> u32 {
    let raw = std::fs::read_to_string(config_path()).unwrap_or_default();
    let map: HashMap<String, toml::Value> = toml::from_str(&raw).unwrap_or_default();
    map.get("remote_quality").and_then(|v| v.as_integer()).map(|v| v as u32).filter(|v| REMOTE_QUALITIES.contains(v)).unwrap_or(0)
}

pub fn save_remote_kbps(kbps: u32) -> std::io::Result<()> {
    save_key("remote_quality", toml::Value::Integer(kbps as i64))
}

/// Ten-band EQ presets (canvas). Off bypasses it (bit-perfect) but keeps
/// the custom curve for later.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum EqPreset {
    #[default]
    Off,
    /// A headphone correction from AutoEq (curve kept separately).
    AutoEq,
    BassBoost,
    Vocal,
    Treble,
    Custom,
}

impl EqPreset {
    pub const ALL: [EqPreset; 6] = [EqPreset::Off, EqPreset::AutoEq, EqPreset::BassBoost, EqPreset::Vocal, EqPreset::Treble, EqPreset::Custom];

    pub fn label(self) -> &'static str {
        match self {
            EqPreset::Off => "Off",
            EqPreset::AutoEq => "AutoEQ",
            EqPreset::BassBoost => "Bass boost",
            EqPreset::Vocal => "Vocal",
            EqPreset::Treble => "Treble",
            EqPreset::Custom => "Custom",
        }
    }

    fn key(self) -> &'static str {
        match self {
            EqPreset::Off => "off",
            EqPreset::AutoEq => "autoeq",
            EqPreset::BassBoost => "bass",
            EqPreset::Vocal => "vocal",
            EqPreset::Treble => "treble",
            EqPreset::Custom => "custom",
        }
    }

    /// The fixed curves (31 Hz .. 16 kHz, dB); None for Off and Custom.
    pub fn curve(self) -> Option<[f32; 10]> {
        match self {
            EqPreset::BassBoost => Some([6.0, 5.0, 4.0, 2.0, 0.5, 0.0, 0.0, 0.0, 0.0, 0.0]),
            EqPreset::Vocal => Some([-2.0, -1.5, -0.5, 0.5, 2.0, 3.0, 3.0, 1.5, 0.0, -1.0]),
            EqPreset::Treble => Some([0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 3.0, 4.5, 5.5]),
            EqPreset::Off | EqPreset::AutoEq | EqPreset::Custom => None,
        }
    }
}

/// Saved EQ preset and custom curve.
pub fn saved_eq() -> (EqPreset, [f32; 10]) {
    let raw = std::fs::read_to_string(config_path()).unwrap_or_default();
    let map: HashMap<String, toml::Value> = toml::from_str(&raw).unwrap_or_default();
    let preset = map.get("eq").and_then(|v| v.as_str()).and_then(|k| EqPreset::ALL.into_iter().find(|p| p.key() == k)).unwrap_or_default();
    let mut gains = [0.0f32; 10];
    if let Some(arr) = map.get("eq_custom").and_then(|v| v.as_array()) {
        for (g, v) in gains.iter_mut().zip(arr) {
            *g = v.as_float().or(v.as_integer().map(|i| i as f64)).filter(|f| f.is_finite()).map(|f| f.clamp(-12.0, 12.0) as f32).unwrap_or(0.0);
        }
    }
    (preset, gains)
}

pub fn save_eq(preset: EqPreset, custom: &[f32; 10]) -> std::io::Result<()> {
    save_key("eq", toml::Value::String(preset.key().into()))?;
    save_key("eq_custom", toml::Value::Array(custom.iter().map(|g| toml::Value::Float(((*g as f64) * 10.0).round() / 10.0)).collect()))
}

/// The saved AutoEQ profile: (headphone name, ten gains).
pub fn saved_autoeq() -> Option<(String, [f32; 10])> {
    let raw = std::fs::read_to_string(config_path()).unwrap_or_default();
    let map: HashMap<String, toml::Value> = toml::from_str(&raw).unwrap_or_default();
    let name = map.get("eq_autoeq_name")?.as_str()?.to_string();
    let arr = map.get("eq_autoeq")?.as_array()?;
    if arr.len() != 10 {
        return None;
    }
    let mut g = [0.0f32; 10];
    for (x, v) in g.iter_mut().zip(arr) {
        *x = v.as_float().or(v.as_integer().map(|i| i as f64)).filter(|f| f.is_finite())?.clamp(-12.0, 12.0) as f32;
    }
    Some((name, g))
}

pub fn save_autoeq(name: &str, gains: &[f32; 10]) -> std::io::Result<()> {
    save_key("eq_autoeq_name", toml::Value::String(name.into()))?;
    save_key("eq_autoeq", toml::Value::Array(gains.iter().map(|g| toml::Value::Float(((*g as f64) * 10.0).round() / 10.0)).collect()))
}

pub fn save_leveling(l: Leveling) -> std::io::Result<()> {
    save_key("leveling", toml::Value::String(l.key().into()))
}

pub fn save_volume(v: f32) -> std::io::Result<()> {
    save_key("volume", toml::Value::Float(((v as f64) * 100.0).round() / 100.0))
}

/// Set one key in ~/.config/crate/config.toml, keeping the others. A file
/// that doesn't parse is left alone (an error, not an empty rewrite), and the
/// write goes through a temp file and rename so a crash can't truncate it.
fn save_key(key: &str, value: toml::Value) -> std::io::Result<()> {
    let path = config_path();
    let raw = match std::fs::read_to_string(&path) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let mut map: toml::Table =
        toml::from_str(&raw).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{} doesn't parse: {e}", path.display())))?;
    map.insert(key.into(), value);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, toml::to_string(&map).unwrap_or_default())?;
    std::fs::rename(&tmp, &path)
}

fn hex(s: &str) -> Option<u32> {
    u32::from_str_radix(s.trim().trim_start_matches('#'), 16).ok()
}

pub fn mix(a: u32, b: u32, t: f32) -> u32 {
    let ch = |c: u32, s: u32| ((c >> s) & 0xff) as f32;
    let m = |s| ((ch(a, s) * (1.0 - t) + ch(b, s) * t).round() as u32) << s;
    m(16) | m(8) | m(0)
}

/// WCAG relative luminance.
fn luminance(c: u32) -> f32 {
    let lin = |v: u32| {
        let s = v as f32 / 255.0;
        if s <= 0.03928 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * lin((c >> 16) & 0xff) + 0.7152 * lin((c >> 8) & 0xff) + 0.0722 * lin(c & 0xff)
}

pub fn contrast(a: u32, b: u32) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// Hue in degrees (None for near-greys).
fn hue(c: u32) -> Option<f32> {
    let (r, g, b) = (((c >> 16) & 0xff) as f32 / 255.0, ((c >> 8) & 0xff) as f32 / 255.0, (c & 0xff) as f32 / 255.0);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let d = max - min;
    if d < 0.08 {
        return None;
    }
    let h = if max == r {
        60.0 * (((g - b) / d) % 6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    Some(if h < 0.0 { h + 360.0 } else { h })
}

fn hue_gap(a: u32, b: u32) -> f32 {
    match (hue(a), hue(b)) {
        (Some(x), Some(y)) => {
            let d = (x - y).abs();
            d.min(360.0 - d)
        }
        _ => 180.0,
    }
}

impl Theme {
    /// The design canvas palette (Tokens.dc.html).
    pub fn crate_default() -> Self {
        Theme {
            base: 0x16161e,
            surface: 0x1a1b26,
            raised: 0x24283b,
            border: 0x2a2e42,
            text: 0xc0caf5,
            text_strong: 0xffffff,
            text2: 0xa9b1d6,
            muted: 0x8b93bd,
            accent: 0x7aa2f7,
            discovery: 0xbb9af7,
            healthy: 0x9ece6a,
            deep: 0x13131a,
            line: 0x33384f,
            red: 0xf7768e,
            orange: 0xff9e64,
            yellow: 0xe0af68,
            green: 0x9ece6a,
            cyan: 0x7dcfff,
            teal: 0x73daca,
            blue: 0x7aa2f7,
            magenta: 0xbb9af7,
        }
    }

    /// The theme for `mode`, plus the Omarchy generation it was read at.
    pub fn load(mode: Mode) -> (Self, String) {
        let gen_id = generation();
        match mode {
            Mode::Crate => (Self::crate_default(), gen_id),
            Mode::Omarchy => (Self::from_omarchy(), gen_id),
        }
    }

    /// Omarchy themes describe terminals; map them onto UI roles with the
    /// contrast and color the design has: near-white titles, readable
    /// secondary text (not the dim "comment" foreground), a muted grey that
    /// passes 4.5:1, distinct accent/discovery/healthy hues, clear layer steps.
    fn from_omarchy() -> Self {
        let raw = std::fs::read_to_string(state_dir().join("theme/colors.toml")).unwrap_or_default();
        let map: HashMap<String, toml::Value> = toml::from_str(&raw).unwrap_or_default();
        let get = |k: &str| map.get(k).and_then(|v| v.as_str()).and_then(hex);
        let d = Self::crate_default();

        let bg = get("background").unwrap_or(d.surface);
        let fg = get("foreground").unwrap_or(d.text);
        let light = luminance(bg) > 0.4;
        let ink = if light { 0x000000 } else { 0xffffff };
        let accent = get("accent").or_else(|| get("blue")).unwrap_or(d.accent);
        let pal = |k: &str, alt: &str, dflt: u32| get(k).or_else(|| get(alt)).unwrap_or(dflt);
        let (red, yellow, green, cyan, blue, magenta) = (
            pal("red", "color1", d.red),
            pal("yellow", "color3", d.yellow),
            pal("green", "color2", d.green),
            pal("cyan", "color6", d.cyan),
            pal("blue", "color4", d.blue),
            pal("magenta", "color5", d.magenta),
        );
        let orange = get("orange").unwrap_or(mix(red, yellow, 0.5));
        let teal = mix(cyan, green, 0.5);

        // Discovery: the theme's magenta unless it reads as the accent; then the
        // candidate furthest from it.
        let discovery = if hue_gap(magenta, accent) >= 30.0 {
            magenta
        } else {
            *[cyan, red, yellow, green].iter().max_by(|a, b| hue_gap(**a, accent).total_cmp(&hue_gap(**b, accent))).unwrap()
        };
        let healthy = if hue_gap(green, accent) >= 25.0 { green } else { teal };

        let surface = bg;
        let raised = get("lighter_background").filter(|&c| contrast(c, bg) > 1.12).unwrap_or(mix(bg, fg, 0.1));
        let mut muted = mix(bg, fg, 0.55);
        let mut t = 0.55;
        while contrast(muted, surface) < 4.5 && t < 0.95 {
            t += 0.05;
            muted = mix(bg, fg, t);
        }
        Theme {
            base: get("dark_background").unwrap_or(mix(bg, 0x000000, 0.2)),
            surface,
            raised,
            border: mix(bg, fg, 0.12),
            text: fg,
            text_strong: mix(fg, ink, 0.6),
            text2: mix(fg, bg, 0.18),
            muted,
            accent,
            discovery,
            healthy,
            deep: get("darker_background").unwrap_or(mix(bg, 0x000000, 0.32)),
            line: mix(bg, fg, 0.2),
            red,
            orange,
            yellow,
            green,
            cyan,
            teal,
            blue,
            magenta,
        }
    }
}
