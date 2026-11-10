//! Color roles from the active Omarchy theme.
//! Source: ~/.local/state/omarchy/current/theme/colors.toml (Omarchy v4).
//! `theme.generation` beside it changes on every theme switch.

use std::{collections::HashMap, path::PathBuf};

#[derive(Clone, Debug)]
pub struct Theme {
    pub base: u32,
    pub surface: u32,
    pub raised: u32,
    pub border: u32,
    pub text: u32,
    pub text_strong: u32,
    pub muted: u32,
    pub accent: u32,
}

fn state_dir() -> PathBuf {
    if let Ok(d) = std::env::var("AULOS_THEME_DIR") {
        return PathBuf::from(d);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".local/state/omarchy/current")
}

pub fn generation() -> String {
    std::fs::read_to_string(state_dir().join("theme.generation"))
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn hex(s: &str) -> Option<u32> {
    u32::from_str_radix(s.trim().trim_start_matches('#'), 16).ok()
}

fn mix(a: u32, b: u32, t: f32) -> u32 {
    let ch = |c: u32, s: u32| ((c >> s) & 0xff) as f32;
    let m = |s| ((ch(a, s) * (1.0 - t) + ch(b, s) * t).round() as u32) << s;
    m(16) | m(8) | m(0)
}

impl Theme {
    pub fn load() -> (Self, String) {
        let gen_id = generation();
        let raw = std::fs::read_to_string(state_dir().join("theme/colors.toml")).unwrap_or_default();
        let map: HashMap<String, toml::Value> = toml::from_str(&raw).unwrap_or_default();
        let get = |k: &str| map.get(k).and_then(|v| v.as_str()).and_then(hex);

        let bg = get("background").unwrap_or(0x1a1b26);
        let fg = get("foreground").unwrap_or(0xc0caf5);
        let t = Theme {
            base: get("dark_background").unwrap_or(mix(bg, 0x000000, 0.15)),
            surface: bg,
            raised: get("lighter_background").unwrap_or(mix(bg, fg, 0.12)),
            border: mix(bg, fg, 0.14),
            text: get("dark_foreground").unwrap_or(fg),
            text_strong: get("bright_foreground").unwrap_or(fg),
            muted: mix(bg, fg, 0.55),
            accent: get("accent").or_else(|| get("blue")).unwrap_or(0x7aa2f7),
        };
        (t, gen_id)
    }
}
