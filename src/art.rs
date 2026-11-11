//! Generated artwork from the design canvas's `.cv-*` patterns: station marks
//! and placeholders for missing covers. Built as full-color SVGs from the
//! theme palette (GPUI renders SVG images with resvg), cached per theme.

use std::{collections::HashMap, fmt::Write as _, sync::Arc};

use gpui::{Image, ImageFormat};

use crate::theme::{Theme, mix};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Motif {
    /// cv-f: concentric rings.
    Rings,
    /// cv-c: a conic sweep.
    Sweep,
    /// cv-b: vertical stripes.
    Stripes,
    /// cv-i: rings radiating from a corner.
    Arcs,
    /// cv-h: two soft glows.
    Glow,
    /// cv-d: a sun setting into a dusk gradient.
    Sun,
    /// cv-a: a dot on a dark gradient.
    Dot,
    /// cv-e: diagonal checks.
    Checks,
    /// cv-g: angled bands.
    Slope,
    /// cv-j: three vertical blocks.
    Blocks,
    /// cv-k: a quartered disc.
    Quarters,
    /// cv-l: a moon over a night gradient.
    Moon,
}

pub const PLACEHOLDERS: [Motif; 12] = [
    Motif::Sun,
    Motif::Stripes,
    Motif::Sweep,
    Motif::Dot,
    Motif::Checks,
    Motif::Rings,
    Motif::Slope,
    Motif::Glow,
    Motif::Arcs,
    Motif::Blocks,
    Motif::Quarters,
    Motif::Moon,
];

fn c(x: u32) -> String {
    format!("#{x:06x}")
}

/// The SVG for `m` in theme `t`, 256×256.
pub fn svg(m: Motif, t: &Theme) -> String {
    let bg = t.surface;
    let mut s = String::from(r#"<svg xmlns="http://www.w3.org/2000/svg" width="256" height="256" viewBox="0 0 256 256">"#);
    match m {
        Motif::Rings => {
            let col = t.magenta;
            let _ = write!(s, r#"<rect width="256" height="256" fill="{}"/>"#, c(bg));
            for (r, w) in [(118.0, 4.0), (78.0, 6.0)] {
                let _ = write!(s, r#"<circle cx="128" cy="128" r="{r}" fill="none" stroke="{}" stroke-width="{w}"/>"#, c(col));
            }
            let _ = write!(s, r#"<circle cx="128" cy="128" r="22" fill="{}"/>"#, c(col));
        }
        Motif::Sweep => {
            // Conic gradient (cv-c) as 72 wedges around (180,154).
            let (a, b) = (t.cyan, mix(t.cyan, t.teal, 0.5));
            let _ = write!(s, r#"<rect width="256" height="256" fill="{}"/>"#, c(bg));
            let (cx, cy, r) = (180.0f32, 154.0f32, 400.0f32);
            for i in 0..72 {
                let f = i as f32 / 72.0;
                // cyan → background (40%) → teal (70%) → cyan
                let col = if f < 0.4 {
                    mix(a, bg, f / 0.4)
                } else if f < 0.7 {
                    mix(bg, b, (f - 0.4) / 0.3)
                } else {
                    mix(b, a, (f - 0.7) / 0.3)
                };
                let (a0, a1) = ((200.0 + f * 360.0).to_radians(), (200.0 + (f + 1.0 / 72.0) * 360.0 + 0.5).to_radians());
                let _ = write!(
                    s,
                    r#"<path d="M{cx} {cy}L{:.1} {:.1}L{:.1} {:.1}Z" fill="{}"/>"#,
                    cx + r * a0.sin(),
                    cy - r * a0.cos(),
                    cx + r * a1.sin(),
                    cy - r * a1.cos(),
                    c(col)
                );
            }
        }
        Motif::Stripes => {
            let _ = write!(s, r#"<rect width="256" height="256" fill="{}"/>"#, c(t.raised));
            let mut x = 0.0;
            while x < 256.0 {
                let _ = write!(s, r#"<rect x="{x}" y="0" width="44" height="256" fill="{}"/>"#, c(t.yellow));
                x += 102.4;
            }
        }
        Motif::Arcs => {
            let _ = write!(s, r#"<rect width="256" height="256" fill="{}"/>"#, c(bg));
            let mut r = 34.0;
            while r < 380.0 {
                let _ = write!(s, r#"<circle cx="0" cy="0" r="{r}" fill="none" stroke="{}" stroke-width="22"/>"#, c(t.teal));
                r += 52.0;
            }
        }
        Motif::Glow => {
            let _ = write!(
                s,
                r#"<defs><radialGradient id="a" cx="20%" cy="80%" r="60%"><stop offset="0" stop-color="{0}"/><stop offset="1" stop-color="{0}" stop-opacity="0"/></radialGradient><radialGradient id="b" cx="80%" cy="20%" r="60%"><stop offset="0" stop-color="{1}"/><stop offset="1" stop-color="{1}" stop-opacity="0"/></radialGradient></defs><rect width="256" height="256" fill="{2}"/><rect width="256" height="256" fill="url(#a)"/><rect width="256" height="256" fill="url(#b)"/>"#,
                c(t.red),
                c(t.blue),
                c(t.base)
            );
        }
        Motif::Sun => {
            let _ = write!(
                s,
                r#"<defs><linearGradient id="g" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="{}"/><stop offset="1" stop-color="{}"/></linearGradient></defs><rect width="256" height="256" fill="url(#g)"/><circle cx="128" cy="256" r="77" fill="{}"/>"#,
                c(mix(t.magenta, t.base, 0.7)),
                c(t.base),
                c(t.orange)
            );
        }
        Motif::Dot => {
            let _ = write!(
                s,
                r#"<defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="{}"/><stop offset="1" stop-color="{}"/></linearGradient></defs><rect width="256" height="256" fill="url(#g)"/><circle cx="77" cy="90" r="46" fill="{}"/>"#,
                c(mix(t.magenta, t.base, 0.8)),
                c(mix(t.magenta, t.base, 0.6)),
                c(t.red)
            );
        }
        Motif::Checks => {
            let _ = write!(s, r#"<rect width="256" height="256" fill="{}"/>"#, c(mix(t.green, t.base, 0.85)));
            for row in 0..8 {
                for col in 0..8 {
                    if (row + col) % 2 == 0 {
                        let _ = write!(s, r#"<rect x="{}" y="{}" width="32" height="32" fill="{}"/>"#, col * 32, row * 32, c(t.green));
                    }
                }
            }
        }
        Motif::Slope => {
            let _ = write!(
                s,
                r#"<rect width="256" height="256" fill="{}"/><path d="M0 0H256V40L0 180Z" fill="{}"/><path d="M0 180L256 40V150L0 290Z" fill="{}"/>"#,
                c(t.raised),
                c(t.text),
                c(mix(t.text, t.base, 0.55))
            );
        }
        Motif::Blocks => {
            let _ = write!(
                s,
                r#"<rect width="86" height="256" fill="{}"/><rect x="85" width="86" height="256" fill="{}"/><rect x="170" width="86" height="256" fill="{}"/>"#,
                c(t.base),
                c(t.yellow),
                c(t.red)
            );
        }
        Motif::Quarters => {
            let _ = write!(
                s,
                r#"<rect width="256" height="256" fill="{3}"/><path d="M128 128V0A128 128 0 0 1 256 128Z" fill="{0}"/><path d="M128 128H256A128 128 0 0 1 128 256Z" fill="{1}"/><path d="M128 128V256A128 128 0 0 1 0 128Z" fill="{2}"/><path d="M128 128H0A128 128 0 0 1 128 0Z" fill="{1}"/>"#,
                c(t.orange),
                c(t.raised),
                c(t.blue),
                c(t.raised)
            );
        }
        Motif::Moon => {
            let _ = write!(
                s,
                r#"<defs><linearGradient id="g" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="{}"/><stop offset="1" stop-color="{}"/></linearGradient></defs><rect width="256" height="256" fill="url(#g)"/><circle cx="166" cy="102" r="31" fill="{}"/>"#,
                c(mix(t.blue, t.base, 0.85)),
                c(mix(t.blue, t.base, 0.65)),
                c(mix(t.text_strong, t.text, 0.3))
            );
        }
    }
    s.push_str("</svg>");
    s
}

/// Rendered images, cached per motif for the current theme.
#[derive(Default)]
pub struct Marks {
    cache: HashMap<Motif, Arc<Image>>,
}

impl Marks {
    pub fn get(&mut self, m: Motif, t: &Theme) -> Arc<Image> {
        self.cache.entry(m).or_insert_with(|| Arc::new(Image::from_bytes(ImageFormat::Svg, svg(m, t).into_bytes()))).clone()
    }

    pub fn clear(&mut self) {
        self.cache.clear();
    }
}

/// A stable placeholder motif for an item without art.
pub fn placeholder_for(key: &str) -> Motif {
    let h = key.bytes().fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
    PLACEHOLDERS[h as usize % PLACEHOLDERS.len()]
}

/// The design's mark for a Plex station, by title.
pub fn station_motif(title: &str) -> Motif {
    match title {
        "Library Radio" => Motif::Glow,
        "Deep Cuts Radio" => Motif::Rings,
        "Time Travel Radio" => Motif::Sweep,
        "Random Album Radio" => Motif::Stripes,
        _ => Motif::Arcs,
    }
}
