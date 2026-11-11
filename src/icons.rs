//! Icons from the design canvas (24×24 path data), served to GPUI's `svg()`
//! as `icons/<name>.svg`. GPUI renders an svg as a mask tinted by
//! `text_color`, so the stroke/fill color here doesn't matter.

use std::borrow::Cow;

use gpui::{AssetSource, SharedString};

/// (name, path data, filled, stroke width)
const ICONS: &[(&str, &str, bool, f32)] = &[
    ("home", "M3 10.5 12 3l9 7.5V21h-6v-6H9v6H3z", false, 1.9),
    ("search", "M11 4a7 7 0 1 0 0 14 7 7 0 0 0 0-14zM20 20l-4-4", false, 2.0),
    ("stations", "M4 9h16v11H4zM8 5l8-3M14 13h3M14 16h3M9.5 13a1.5 1.5 0 1 0 0 3 1.5 1.5 0 0 0 0-3z", false, 1.9),
    ("artists", "M12 12a4 4 0 1 0 0-8 4 4 0 0 0 0 8zM4 21c0-4 4-6 8-6s8 2 8 6", false, 1.9),
    ("albums", "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zM12 10a2 2 0 1 0 0 4 2 2 0 0 0 0-4z", false, 1.9),
    ("tracks", "M9 18V5l11-2v13M9 18a3 3 0 1 1-6 0 3 3 0 0 1 6 0zM20 16a3 3 0 1 1-6 0 3 3 0 0 1 6 0z", false, 1.9),
    ("playlists", "M3 6h12M3 12h12M3 18h7M16 14l5 3-5 3z", false, 1.9),
    ("genres", "M3 12V3h9l9 9-9 9zM7.5 7.5h.01", false, 1.9),
    ("history", "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zM12 7v5l3 3", false, 1.9),
    ("offline", "M12 3v12M7 10l5 5 5-5M4 21h16", false, 1.9),
    ("plus", "M12 5v14M5 12h14", false, 2.2),
    ("bolt", "M13 2 4 14h7l-1 8 9-12h-7z", false, 2.4),
    ("server", "M4 5h16v6H4zM4 13h16v6H4zM8 8h.01M8 16h.01", false, 2.0),
    ("updown", "M7 10l5-5 5 5M7 14l5 5 5-5", false, 2.0),
    ("back", "M15 5l-7 7 7 7", false, 2.0),
    ("forward", "M9 5l7 7-7 7", false, 2.0),
    ("shuffle", "M3 7h3c5 0 7 10 12 10h3M3 17h3c2 0 3.5-1.5 5-4M14 9c1-1.3 2.3-2 4-2h3M18 4l3 3-3 3M18 14l3 3-3 3", false, 2.0),
    ("repeat", "M4 11V9a3 3 0 0 1 3-3h13M17 3l3 3-3 3M20 13v2a3 3 0 0 1-3 3H4M7 21l-3-3 3-3", false, 2.0),
    ("prev", "M19 5v14L8 12zM5 5h2.5v14H5z", true, 0.0),
    ("next", "M5 5v14l11-7zM16.5 5H19v14h-2.5z", true, 0.0),
    ("play", "M7 4v16l13-8z", true, 0.0),
    ("pause", "M7 4h3.5v16H7zM13.5 4H17v16h-3.5z", true, 0.0),
    ("star", "M12 3l2.8 5.8 6.2.9-4.5 4.4 1 6.2L12 17.4 6.5 20.3l1-6.2L3 9.7l6.2-.9z", true, 0.0),
    ("star-outline", "M12 3l2.8 5.8 6.2.9-4.5 4.4 1 6.2L12 17.4 6.5 20.3l1-6.2L3 9.7l6.2-.9z", false, 2.0),
    ("lyrics", "M12 3a3 3 0 0 0-3 3v6a3 3 0 0 0 6 0V6a3 3 0 0 0-3-3zM5 11a7 7 0 0 0 14 0M12 18v3", false, 2.0),
    ("queue", "M3 6h18M3 12h18M3 18h11M18 15v6M15 18h6", false, 2.0),
    ("device", "M6 3h12v18H6zM12 7h.01M12 11a3 3 0 1 0 0 6 3 3 0 0 0 0-6z", false, 2.0),
    ("volume", "M4 9v6h4l5 4V5L8 9zM16 9a4 4 0 0 1 0 6", false, 2.0),
    ("mini", "M3 5h18v14H3zM12 12h7v5h-7z", false, 2.0),
    ("sonic", "M5 19a2 2 0 1 0 0-4 2 2 0 0 0 0 4zM19 9a2 2 0 1 0 0-4 2 2 0 0 0 0 4zM7 17c6 0 4-10 10-10", false, 2.0),
    ("more", "M5 12h.01M12 12h.01M19 12h.01", false, 3.0),
    ("heart", "M12 20s-7-4.4-7-10a4 4 0 0 1 7-2.6A4 4 0 0 1 19 10c0 5.6-7 10-7 10z", false, 2.0),
    ("down", "M6 9l6 6 6-6", false, 2.0),
    ("close", "M6 6l12 12M18 6 6 18", false, 2.5),
    ("clock", "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zM12 7v5l3 2", false, 2.0),
];

fn svg_for(name: &str) -> Option<String> {
    // The Crate mark (two records in a crate on an accent tile), as layers:
    // GPUI tints each svg one color, so `logo()` stacks accent and base layers.
    if let Some(i) = name.strip_prefix("crate-").and_then(|n| n.parse::<usize>().ok()) {
        const L: [&str; 7] = [
            r#"<rect x="0" y="0" width="32" height="32" rx="8" fill="black"/>"#,
            r#"<circle cx="19.6" cy="12" r="5.6" fill="black"/>"#,
            r#"<circle cx="19.6" cy="12" r="5.6" fill="none" stroke="black" stroke-width="1.4"/>"#,
            r#"<circle cx="12.6" cy="12.6" r="5.6" fill="black"/>"#,
            r#"<circle cx="12.6" cy="12.6" r="5.6" fill="none" stroke="black" stroke-width="1.4"/><circle cx="12.6" cy="12.6" r="1.5" fill="black"/>"#,
            r#"<rect x="5.5" y="15" width="21" height="11.5" rx="2.2" fill="black"/>"#,
            r#"<rect x="5.5" y="15" width="21" height="11.5" rx="2.2" fill="none" stroke="black" stroke-width="1.4"/><rect x="12" y="18.4" width="8" height="2.8" rx="1.4" fill="black"/>"#,
        ];
        return L.get(i).map(|l| format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32" viewBox="0 0 32 32">{l}</svg>"#));
    }
    let &(_, d, filled, w) = ICONS.iter().find(|i| i.0 == name)?;
    Some(if filled {
        format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24"><path d="{d}" fill="black"/></svg>"#)
    } else {
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none"><path d="{d}" stroke="black" stroke-width="{w}" stroke-linecap="round" stroke-linejoin="round"/></svg>"#
        )
    })
}

pub struct Icons;

impl AssetSource for Icons {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        let name = path.strip_prefix("icons/").and_then(|p| p.strip_suffix(".svg"));
        Ok(name.and_then(svg_for).map(|s| Cow::Owned(s.into_bytes())))
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        if path.starts_with("icons") {
            Ok(ICONS.iter().map(|i| SharedString::from(format!("icons/{}.svg", i.0))).collect())
        } else {
            Ok(vec![])
        }
    }
}

/// The Crate logo at `size`: accent tile, records and crate in `base`.
pub fn logo(size: f32, accent: u32, base: u32) -> gpui::Div {
    use gpui::{ParentElement, Styled, div, px, rgb};
    div().size(px(size)).flex_none().relative().children((0..7).map(move |i| {
        gpui::svg()
            .path(format!("icons/crate-{i}.svg"))
            .absolute()
            .size(px(size))
            .text_color(rgb(if i % 2 == 0 { accent } else { base }))
    }))
}

/// `icon("home", 18.0, color)`
pub fn icon(name: &'static str, size: f32, color: u32) -> gpui::Svg {
    use gpui::{Styled, px, rgb};
    gpui::svg().path(format!("icons/{name}.svg")).size(px(size)).flex_none().text_color(rgb(color))
}
