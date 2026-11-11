//! Gapless MP4/AAC. Plex's AAC files (ffmpeg-encoded) carry the encoder
//! delay and the true length only in the MP4 edit list (`elst`): media time
//! 2048 = samples of priming to drop, segment duration = the real length in
//! the movie's timescale (`mvhd`). symphonia parses the edit list but drops
//! it, and its AAC decoder ignores packet trims, so the engine applies it.
//!
//! [`Sniff`] records the bytes symphonia reads while it probes the file (it
//! has to read `moov`), and [`edit_list`] walks those bytes as an atom tree,
//! picks the `trak` symphonia chose, and accepts only a plain edit list (one
//! entry at normal rate), else nothing. [`EditList`] then trims the audio.

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use symphonia::core::io::MediaSource;

/// Most bytes kept while probing (moov is small; this bounds odd files).
const CAPTURE_MAX: usize = 16 << 20;

/// Bytes read during the probe, by file offset.
#[derive(Default)]
pub struct Captured {
    chunks: BTreeMap<u64, Vec<u8>>,
    total: usize,
}

impl Captured {
    fn add(&mut self, at: u64, bytes: &[u8]) {
        if self.total + bytes.len() > CAPTURE_MAX {
            return;
        }
        // A shorter re-read at the same offset keeps the longer one.
        if self.chunks.get(&at).is_some_and(|c| c.len() >= bytes.len()) {
            return;
        }
        self.total += bytes.len();
        self.chunks.insert(at, bytes.to_vec());
    }

    /// `len` bytes at `at`, joining consecutive reads.
    fn get(&self, at: u64, len: usize) -> Option<Vec<u8>> {
        let mut out = Vec::with_capacity(len);
        let mut pos = at;
        while out.len() < len {
            // Any read that covers `pos` (reads can overlap after seeks).
            let (start, chunk) = self.chunks.range(..=pos).rev().find(|(s, c)| pos < **s + c.len() as u64)?;
            let off = (pos - start) as usize;
            let take = (len - out.len()).min(chunk.len() - off);
            out.extend_from_slice(&chunk[off..off + take]);
            pos += take as u64;
        }
        Some(out)
    }
}

/// A MediaSource that records what's read until told to stop.
pub struct Sniff<S> {
    inner: S,
    pos: u64,
    captured: Arc<Mutex<Captured>>,
    recording: Arc<AtomicBool>,
}

/// The handle the engine keeps: stop recording, then look for an edit list.
#[derive(Clone)]
pub struct Probe {
    captured: Arc<Mutex<Captured>>,
    recording: Arc<AtomicBool>,
}

impl Probe {
    /// Stop recording and let the capture go (not an AAC track).
    pub fn stop(&self) {
        self.recording.store(false, Ordering::Release);
        drop(std::mem::take(&mut *self.captured.lock().unwrap()));
    }

    /// Stop recording (the probe is done) and find `track_id`'s edit list:
    /// (priming frames, real length in frames) at `rate`.
    pub fn trims(&self, track_id: u32, rate: u32) -> Option<(u64, u64)> {
        self.recording.store(false, Ordering::Release);
        let c = std::mem::take(&mut *self.captured.lock().unwrap());
        edit_list(&c, track_id)?.trims(rate)
    }
}

impl<S> Sniff<S> {
    pub fn new(inner: S) -> (Self, Probe) {
        let captured = Arc::new(Mutex::new(Captured::default()));
        let recording = Arc::new(AtomicBool::new(true));
        (Sniff { inner, pos: 0, captured: captured.clone(), recording: recording.clone() }, Probe { captured, recording })
    }
}

impl<S: Read> Read for Sniff<S> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(out)?;
        if n > 0 && self.recording.load(Ordering::Acquire) {
            self.captured.lock().unwrap().add(self.pos, &out[..n]);
        }
        self.pos += n as u64;
        Ok(n)
    }
}

impl<S: Seek> Seek for Sniff<S> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.pos = self.inner.seek(pos)?;
        Ok(self.pos)
    }
}

impl MediaSource for Sniff<Box<dyn MediaSource>> {
    fn is_seekable(&self) -> bool {
        self.inner.is_seekable()
    }
    fn byte_len(&self) -> Option<u64> {
        self.inner.byte_len()
    }
}

/// One audio track's edit list, as far as gapless needs it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Edit {
    pub movie_timescale: u32,
    pub media_timescale: u32,
    pub segment_duration: u64,
    pub media_time: i64,
}

impl Edit {
    /// (priming frames to drop, real length in frames) at `rate`.
    pub fn trims(&self, rate: u32) -> Option<(u64, u64)> {
        if self.movie_timescale == 0 || self.media_timescale == 0 || self.media_time < 0 || self.segment_duration == 0 {
            return None;
        }
        // Rounded to the nearest frame, as ffmpeg does.
        let ts = self.movie_timescale as u128;
        let total = (self.segment_duration as u128 * rate as u128 + ts / 2) / ts;
        let mts = self.media_timescale as u128;
        let delay = (self.media_time as u128 * rate as u128 + mts / 2) / mts;
        Some((u64::try_from(delay).ok()?, u64::try_from(total).ok()?))
    }
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes(b[..4].try_into().unwrap())
}

fn be64(b: &[u8]) -> u64 {
    u64::from_be_bytes(b[..8].try_into().unwrap())
}

/// An atom header at `at`: (type, header length, total size; None = to end).
fn header(c: &Captured, at: u64) -> Option<([u8; 4], u64, Option<u64>)> {
    let h = c.get(at, 8)?;
    let size = be32(&h) as u64;
    let kind: [u8; 4] = h[4..8].try_into().ok()?;
    match size {
        0 => Some((kind, 8, None)),
        1 => {
            let big = be64(&c.get(at + 8, 8)?);
            (big >= 16).then_some((kind, 16, Some(big)))
        }
        s if s >= 8 => Some((kind, 8, Some(s))),
        _ => None,
    }
}

/// The child atoms in `[start, end)`: (type, body start, body end).
fn children(c: &Captured, start: u64, end: u64) -> Vec<([u8; 4], u64, u64)> {
    let mut out = Vec::new();
    let mut at = start;
    while at + 8 <= end {
        let Some((kind, hl, size)) = header(c, at) else { break };
        let stop = size.map(|s| at.saturating_add(s)).unwrap_or(end).min(end);
        if stop < at + hl {
            break;
        }
        out.push((kind, at + hl, stop));
        if size.is_none() {
            break;
        }
        at = stop;
    }
    out
}

/// Walk moov: the edit list of the audio trak with `track_id`, if it's a
/// single normal-rate edit.
pub fn edit_list(c: &Captured, track_id: u32) -> Option<Edit> {
    // Top level: skip mdat and friends by size; find moov.
    let mut at = 0u64;
    let moov = loop {
        let (kind, hl, size) = header(c, at)?;
        if &kind == b"moov" {
            break (at + hl, at.checked_add(size?)?);
        }
        at = at.checked_add(size?)?;
    };
    // mvhd may come before or after the traks.
    let kids = children(c, moov.0, moov.1);
    let (_, ms, _) = kids.iter().find(|k| &k.0 == b"mvhd")?;
    let v = c.get(*ms, 1)?[0];
    let movie_timescale = be32(&c.get(ms + if v == 1 { 20 } else { 12 }, 4)?);
    kids.iter().filter(|k| &k.0 == b"trak").find_map(|(_, s, e)| trak(c, *s, *e, track_id, movie_timescale))
}

fn trak(c: &Captured, s: u64, e: u64, track_id: u32, movie_timescale: u32) -> Option<Edit> {
    let kids = children(c, s, e);
    // tkhd: the track id (v0 at 12, v1 at 20, after version/flags and times).
    let (_, ts, _) = kids.iter().find(|k| &k.0 == b"tkhd")?;
    let v = c.get(*ts, 1)?[0];
    let id = be32(&c.get(ts + if v == 1 { 20 } else { 12 }, 4)?);
    if id != track_id {
        return None;
    }
    // mdia: an audio handler and the media timescale.
    let (_, ms, me) = kids.iter().find(|k| &k.0 == b"mdia")?;
    let mdia = children(c, *ms, *me);
    let (_, hs, _) = mdia.iter().find(|k| &k.0 == b"hdlr")?;
    if c.get(hs + 8, 4)? != b"soun" {
        return None;
    }
    let (_, ds, _) = mdia.iter().find(|k| &k.0 == b"mdhd")?;
    let v = c.get(*ds, 1)?[0];
    let media_timescale = be32(&c.get(ds + if v == 1 { 20 } else { 12 }, 4)?);
    // edts/elst: exactly one entry at rate 1.0 (anything fancier: no trims).
    let (_, es, ee) = kids.iter().find(|k| &k.0 == b"edts")?;
    let (_, ls, _) = children(c, *es, *ee).into_iter().find(|k| &k.0 == b"elst")?;
    let head = c.get(ls, 8)?;
    let (v, count) = (head[0], be32(&head[4..]));
    if count != 1 {
        return None;
    }
    let (segment_duration, media_time, rate) = if v == 1 {
        let b = c.get(ls + 8, 20)?;
        (be64(&b), be64(&b[8..]) as i64, be32(&b[16..]))
    } else {
        let b = c.get(ls + 8, 12)?;
        (be32(&b) as u64, be32(&b[4..]) as i32 as i64, be32(&b[8..]))
    };
    if rate != 0x0001_0000 {
        return None;
    }
    Some(Edit { movie_timescale, media_timescale, segment_duration, media_time })
}

/// Applies an edit list to decoded audio: drops the priming frames, then
/// everything past the real length.
pub struct EditList {
    /// Priming frames still to drop.
    pub skip: u64,
    /// Frames still allowed out (None: no limit).
    pub left: Option<u64>,
}

impl EditList {
    /// For a track decoded from the start (`seeked` false), or from a seek
    /// to `start_frames` into the real audio (the seek's packet lead is
    /// dropped before this sees the audio).
    pub fn new(delay: u64, total: u64, seeked: bool, start_frames: u64) -> Self {
        if seeked {
            EditList { skip: 0, left: Some(total.saturating_sub(start_frames)) }
        } else {
            EditList { skip: delay, left: Some(total) }
        }
    }

    /// Trim interleaved `buf` (`ch` channels) in place.
    pub fn apply(&mut self, buf: &mut Vec<f32>, ch: usize) {
        let frames = (buf.len() / ch) as u64;
        let drop = self.skip.min(frames);
        if drop > 0 {
            buf.drain(..drop as usize * ch);
            self.skip -= drop;
        }
        if let Some(left) = self.left.as_mut() {
            let have = (buf.len() / ch) as u64;
            if have > *left {
                buf.truncate(*left as usize * ch);
            }
            *left -= (buf.len() / ch) as u64;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn atom(tag: &[u8], body: &[u8]) -> Vec<u8> {
        let mut v = ((8 + body.len()) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(tag);
        v.extend_from_slice(body);
        v
    }

    fn mvhd(ts: u32) -> Vec<u8> {
        let mut b = vec![0, 0, 0, 0];
        b.extend_from_slice(&[0; 8]);
        b.extend_from_slice(&ts.to_be_bytes());
        b.extend_from_slice(&[0; 80]);
        atom(b"mvhd", &b)
    }

    fn trak(id: u32, handler: &[u8; 4], media_ts: u32, edits: &[(u32, i32)]) -> Vec<u8> {
        let mut tkhd = vec![0, 0, 0, 0];
        tkhd.extend_from_slice(&[0; 8]);
        tkhd.extend_from_slice(&id.to_be_bytes());
        tkhd.extend_from_slice(&[0; 60]);
        let mut elst = vec![0, 0, 0, 0];
        elst.extend_from_slice(&(edits.len() as u32).to_be_bytes());
        for (d, m) in edits {
            elst.extend_from_slice(&d.to_be_bytes());
            elst.extend_from_slice(&m.to_be_bytes());
            elst.extend_from_slice(&[0, 1, 0, 0]);
        }
        let mut hdlr = vec![0; 8];
        hdlr.extend_from_slice(handler);
        hdlr.extend_from_slice(&[0; 12]);
        let mut mdhd = vec![0, 0, 0, 0];
        mdhd.extend_from_slice(&[0; 8]);
        mdhd.extend_from_slice(&media_ts.to_be_bytes());
        mdhd.extend_from_slice(&[0; 8]);
        let mdia = [atom(b"hdlr", &hdlr), atom(b"mdhd", &mdhd)].concat();
        let body = [atom(b"tkhd", &tkhd), atom(b"edts", &atom(b"elst", &elst)), atom(b"mdia", &mdia)].concat();
        atom(b"trak", &body)
    }

    fn capture(file: &[u8]) -> Captured {
        let src: Box<dyn MediaSource> = Box::new(std::io::Cursor::new(file.to_vec()));
        let (mut s, probe) = Sniff::new(src);
        let mut out = [0u8; 7]; // tiny reads: atoms straddle them
        while s.read(&mut out).unwrap() > 0 {}
        std::mem::take(&mut *probe.captured.lock().unwrap())
    }

    #[test]
    fn picks_the_selected_audio_trak_and_skips_lookalikes() {
        // A free box whose payload looks like mvhd/elst, mdat, then moov with
        // a video trak before the real audio trak.
        let mut fake = mvhd(1);
        fake.extend(atom(b"elst", &[0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 1, 0, 0]));
        let moov = [mvhd(1000), trak(1, b"vide", 90000, &[(1000, 0)]), trak(2, b"soun", 44100, &[(247_973, 2048)])].concat();
        let file = [atom(b"ftyp", b"M4A \0\0\0\0"), atom(b"free", &fake), atom(b"mdat", &[7u8; 300]), atom(b"moov", &moov)].concat();
        let c = capture(&file);
        let e = edit_list(&c, 2).expect("audio trak");
        assert_eq!(e.trims(44_100), Some((2048, (247_973 * 44_100 + 500) / 1000)));
        assert_eq!(edit_list(&c, 1), None, "not audio");
    }

    #[test]
    fn overlapping_reads_and_late_mvhd() {
        let mut c = Captured::default();
        c.add(0, &[1u8; 4096]);
        c.add(2048, &[2u8; 512]);
        c.add(0, &[3u8; 1024]);
        assert_eq!(c.get(3000, 8), Some(vec![1u8; 8]));
        let moov = [trak(2, b"soun", 44100, &[(1000, 2048)]), mvhd(1000)].concat();
        let file = [atom(b"ftyp", b"M4A \0\0\0\0"), atom(b"moov", &moov)].concat();
        assert!(edit_list(&capture(&file), 2).is_some());
    }

    #[test]
    fn multi_entry_edit_lists_get_no_trims() {
        let moov = [mvhd(1000), trak(1, b"soun", 48000, &[(1000, 0), (9000, 48000)])].concat();
        let file = [atom(b"ftyp", b"M4A \0\0\0\0"), atom(b"moov", &moov)].concat();
        assert_eq!(edit_list(&capture(&file), 1), None);
    }

    #[test]
    fn edit_list_trims_start_and_end() {
        let mut e = EditList::new(3, 5, false, 0);
        let mut a: Vec<f32> = (0..8).flat_map(|i| [i as f32, i as f32]).collect();
        e.apply(&mut a, 2);
        assert_eq!(a, vec![3.0, 3.0, 4.0, 4.0, 5.0, 5.0, 6.0, 6.0, 7.0, 7.0]);
        let mut b: Vec<f32> = vec![9.0; 6];
        e.apply(&mut b, 2);
        assert!(b.is_empty(), "past the real length");
    }
}

#[cfg(test)]
mod sample_test {
    /// CRATE_MP4_SAMPLE=path: decode with the edit list applied, print the
    /// frame count, and write raw f32 stereo to CRATE_MP4_DUMP if set.
    #[test]
    fn decodes_mp4_sample_with_edit_list() {
        use symphonia::core::codecs::audio::AudioDecoderOptions;
        use symphonia::core::formats::{FormatOptions, TrackType};
        use symphonia::core::io::MediaSourceStream;
        use symphonia::core::meta::MetadataOptions;
        let Some(path) = std::env::var_os("CRATE_MP4_SAMPLE") else { return };
        let file: Box<dyn symphonia::core::io::MediaSource> = Box::new(std::fs::File::open(path).unwrap());
        let (sniff, probe) = super::Sniff::new(file);
        let mss = MediaSourceStream::new(Box::new(sniff), Default::default());
        let mut hint = symphonia::core::formats::probe::Hint::new();
        hint.with_extension("m4a");
        let mut fmt = symphonia::default::get_probe().probe(&hint, mss, FormatOptions::default(), MetadataOptions::default()).unwrap();
        let track = fmt.default_track(TrackType::Audio).unwrap().clone();
        let params = track.codec_params.as_ref().unwrap().audio().unwrap().clone();
        let rate = params.sample_rate.unwrap();
        let (delay, total) = probe.trims(track.id, rate).expect("edit list");
        let mut edit = super::EditList::new(delay, total, false, 0);
        let mut dec = crate::audio::opus::codecs().make_audio_decoder(&params, &AudioDecoderOptions::default()).unwrap();
        let (mut out, mut raw) = (Vec::new(), 0usize);
        while let Ok(Some(p)) = fmt.next_packet() {
            let b = dec.decode(&p).unwrap();
            let mut tmp = vec![0f32; b.frames() * 2];
            b.copy_to_slice_interleaved(&mut tmp[..]);
            raw += b.frames();
            edit.apply(&mut tmp, 2);
            out.extend(tmp);
        }
        eprintln!("delay {delay} total {total} raw {raw} out {}", out.len() / 2);
        if let Some(d) = std::env::var_os("CRATE_MP4_DUMP") {
            std::fs::write(d, out.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>()).unwrap();
        }
        assert_eq!(out.len() / 2, total as usize);
    }
}
