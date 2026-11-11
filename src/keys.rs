//! Canvas keys for lists: j/k move a cursor through the Albums grid or list
//! and an album's tracks, Enter opens or plays it, x marks it, r starts
//! radio from the selection (marked albums, else the cursor, else the
//! album or artist on screen, else the playing track).

use gpui::{Context, point, px};

use crate::{Crate, View};

/// Album track table geometry (album.rs: the `head` row and 52 px track rows).
/// Keep in step with album_view; reveal_track also relies on the album
/// scroll's children being header, actions, body in that order.
const TRACK_HEAD_H: f32 = 25.0;
const TRACK_ROW_H: f32 = 52.0;
/// A DISC header row on multi-disc album pages (album.rs).
pub(crate) const DISC_HEAD_H: f32 = 36.0;

impl Crate {
    /// Length of the list the cursor moves through on this view.
    fn cursor_len(&self) -> usize {
        match &self.view {
            View::Library => self.cells(),
            View::Album { tracks: Some(t), .. } => t.len(),
            _ => 0,
        }
    }

    /// Now Playing covers the lists (it isn't drawn in the compact layout).
    fn overlay_shown(&self) -> bool {
        self.now_playing && self.now.is_some() && !self.compact
    }

    /// Where the cursor starts: the first row on screen, not the top of the list.
    fn first_visible(&self) -> usize {
        match &self.view {
            View::Library => {
                let scrolled: f32 = (-self.scroll.0.borrow().base_handle.offset().y).into();
                let row_h = if self.lib_list { crate::library::LIST_ROW_H } else { self.row_h() };
                let row = (scrolled / row_h.max(1.0) - 0.01).ceil().max(0.0) as usize;
                if self.lib_list { row } else { row * self.cols.max(1) }
            }
            View::Album { .. } => {
                let (Some(body), view) = (self.album_scroll.bounds_for_item(2), self.album_scroll.bounds()) else { return 0 };
                let above: f32 = (view.top() - body.top()).into();
                // The first row whose top is on screen.
                let n = self.album_tracks().map(|t| t.len()).unwrap_or(0);
                (0..n).find(|&i| self.track_y(i) >= above - 0.5).unwrap_or(n.saturating_sub(1))
            }
            _ => 0,
        }
    }

    pub(crate) fn move_cursor(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.overlay_shown() {
            return;
        }
        let len = self.cursor_len();
        if len == 0 {
            return;
        }
        let next = match self.cursor {
            None => self.first_visible().min(len - 1),
            Some(c) => (c as isize + delta).clamp(0, len as isize - 1) as usize,
        };
        self.cursor = Some(next);
        match &self.view {
            View::Library => {
                let row = if self.lib_list { next } else { next / self.cols.max(1) };
                self.scroll.scroll_to_item(row, gpui::ScrollStrategy::Nearest);
            }
            View::Album { .. } => self.reveal_track(next),
            _ => {}
        }
        cx.notify();
    }

    /// Track row `i`'s top below the album body's top, counting the DISC
    /// headers above it.
    fn track_y(&self, i: usize) -> f32 {
        let headers = self.album_tracks().map(|t| crate::album::disc_headers(&t)).unwrap_or_default();
        let above = headers.iter().take(i + 1).filter(|h| **h).count();
        TRACK_HEAD_H + TRACK_ROW_H * i as f32 + DISC_HEAD_H * above as f32
    }

    /// Keep track row `i` inside the album view's viewport.
    fn reveal_track(&self, i: usize) {
        // Children of the album scroll: header, actions, body (table + aside).
        let (Some(body), view) = (self.album_scroll.bounds_for_item(2), self.album_scroll.bounds()) else { return };
        let top = body.top() + px(self.track_y(i));
        let bottom = top + px(TRACK_ROW_H);
        let mut off = self.album_scroll.offset();
        let margin = px(12.0);
        if top < view.top() + margin {
            off.y += view.top() + margin - top;
        } else if bottom > view.bottom() - margin {
            off.y -= bottom - (view.bottom() - margin);
        }
        let max = self.album_scroll.max_offset().y;
        off.y = off.y.min(px(0.0)).max(-max);
        self.album_scroll.set_offset(point(off.x, off.y));
    }

    /// Enter: open the album under the cursor, or play the track under it.
    pub(crate) fn activate_cursor(&mut self, cx: &mut Context<Self>) {
        let Some(c) = self.cursor else { return };
        if self.overlay_shown() {
            return;
        }
        match &self.view {
            View::Library => {
                let ix = self.cell_album(c);
                if self.album(ix).is_some() {
                    self.open_album(ix, cx);
                }
            }
            View::Album { ix, tracks: Some(tracks) } => {
                if c < tracks.len()
                    && let Some(album) = self.album(*ix).cloned()
                {
                    let tracks = tracks.clone();
                    self.play_tracks(album, tracks, c, cx);
                }
            }
            _ => {}
        }
    }

    /// x: mark or unmark the album under the cursor (Albums only).
    pub(crate) fn toggle_cursor_mark(&mut self, cx: &mut Context<Self>) {
        let (View::Library, Some(c)) = (&self.view, self.cursor) else { return };
        let Some(key) = self.album(self.cell_album(c)).map(|a| a.rating_key.clone()) else { return };
        if let Some(p) = self.selection.iter().position(|k| *k == key) {
            self.selection.remove(p);
        } else {
            self.selection.push(key);
        }
        cx.notify();
    }

    /// r: Plex radio from whatever is selected.
    pub(crate) fn radio_from_selection(&mut self, cx: &mut Context<Self>) {
        let mut note = None;
        let seed: Option<(String, String)> = match &self.view {
            _ if self.overlay_shown() => None,
            View::Library if !self.selection.is_empty() => {
                let key = self.selection[0].clone();
                let title = self.albums.iter().find(|a| a.rating_key == key).map(|a| a.title.clone()).unwrap_or_default();
                if self.selection.len() > 1 {
                    // Plex radio takes one seed; say which one was used.
                    note = Some("radio starts from one album: the first marked".to_string());
                }
                self.selection.clear();
                Some((key, title))
            }
            View::Library => self.cursor.and_then(|c| self.album(self.cell_album(c))).map(|a| (a.rating_key.clone(), a.title.clone())),
            View::Album { ix, tracks } => match (self.cursor, tracks) {
                (Some(c), Some(t)) if c < t.len() => Some((t[c].rating_key.clone(), t[c].title.clone())),
                _ => self.album(*ix).map(|a| (a.rating_key.clone(), a.title.clone())),
            },
            View::Artist { key, name, .. } => Some((key.clone(), name.clone())),
            _ => None,
        };
        // Nothing picked on screen (or Now Playing is up): the playing track.
        let seed = seed.or_else(|| self.now.as_ref().and_then(|n| n.tracks.get(n.idx)).map(|t| (t.rating_key.clone(), t.title.clone())));
        match seed {
            Some((key, title)) => {
                let name = format!("{title} Radio");
                // The note shows when this radio's queue starts; any other queue drops it.
                self.play_note = note.map(|n| (name.clone(), n));
                self.play_radio(&key, name, cx);
            }
            None => {
                self.status = "Pick an album or track to start radio from".into();
                cx.notify();
            }
        }
    }
}
