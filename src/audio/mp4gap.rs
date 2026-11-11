//! Gapless MP4/AAC. Plex's AAC files (ffmpeg-encoded) carry the encoder
//! delay and the true length only in the MP4 edit list (`elst`): media time
//! 2048 = samples of priming to drop, segment duration = the real length in
//! the movie's timescale (`mvhd`). symphonia parses the edit list but drops
//! it, and its AAC decoder ignores packet trims, so the engine applies it:
//! [`Sniff`] picks the two atoms out of the bytes symphonia reads, and
//! [`EditList`] says what to drop.

use std::io::{Read, Seek, SeekFrom};
use std::sync::{Arc, Mutex};

use symphonia::core::io::MediaSource;

/// What the edit list says, once both atoms have been seen.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Found {
    /// mvhd timescale (units of the edit's segment duration).
    pub movie_timescale: u32,
    /// First edit: segment duration (movie timescale) and media time
    /// (media timescale, = the sample rate for audio).
    pub segment_duration: u64,
    pub media_time: i64,
    pub have_mvhd: bool,
    pub have_elst: bool,
}

impl Found {
    /// (priming frames to drop, real length in frames) at `rate`.
    pub fn trims(&self, rate: u32) -> Option<(u64, u64)> {
        if !(self.have_mvhd && self.have_elst) || self.movie_timescale == 0 || self.media_time < 0 || self.segment_duration == 0 {
            return None;
        }
        // Rounded to the nearest frame, as ffmpeg does.
        let ts = self.movie_timescale as u128;
        let total = ((self.segment_duration as u128 * rate as u128 + ts / 2) / ts) as u64;
        Some((self.media_time as u64, total))
    }
}

/// A MediaSource that watches the bytes going by for `mvhd` and `elst`.
pub struct Sniff<S> {
    inner: S,
    found: Arc<Mutex<Found>>,
    /// The last bytes read, so an atom split across reads is still seen.
    carry: Vec<u8>,
}

impl<S> Sniff<S> {
    pub fn new(inner: S) -> (Self, Arc<Mutex<Found>>) {
        let found = Arc::new(Mutex::new(Found::default()));
        (Sniff { inner, found: found.clone(), carry: Vec::new() }, found)
    }

    fn scan(&mut self, new: &[u8]) {
        let mut f = self.found.lock().unwrap();
        if f.have_mvhd && f.have_elst {
            return;
        }
        // Look in carry + new; keep the tail for the next read.
        let mut buf = std::mem::take(&mut self.carry);
        buf.extend_from_slice(new);
        let mut i = 4;
        while i + 4 <= buf.len() {
            let tag = &buf[i..i + 4];
            if tag == b"mvhd" && !f.have_mvhd {
                // version/flags, then v0: ctime u32, mtime u32, timescale u32;
                // v1: ctime u64, mtime u64, timescale u32.
                let b = &buf[i + 4..];
                if b.len() >= 24 {
                    let v = b[0];
                    let off = if v == 1 { 4 + 16 } else { 4 + 8 };
                    if b.len() >= off + 4 {
                        f.movie_timescale = u32::from_be_bytes(b[off..off + 4].try_into().unwrap());
                        f.have_mvhd = true;
                    }
                } else {
                    break; // wait for more bytes
                }
            } else if tag == b"elst" && !f.have_elst {
                // version/flags, entry count u32, then the first entry:
                // v0: duration u32, media time i32; v1: u64, i64.
                let b = &buf[i + 4..];
                if b.len() >= 8 + 16 {
                    let v = b[0];
                    let count = u32::from_be_bytes(b[4..8].try_into().unwrap());
                    if count >= 1 {
                        if v == 1 {
                            f.segment_duration = u64::from_be_bytes(b[8..16].try_into().unwrap());
                            f.media_time = i64::from_be_bytes(b[16..24].try_into().unwrap());
                        } else {
                            f.segment_duration = u32::from_be_bytes(b[8..12].try_into().unwrap()) as u64;
                            f.media_time = i32::from_be_bytes(b[12..16].try_into().unwrap()) as i64;
                        }
                        f.have_elst = true;
                    }
                } else {
                    break;
                }
            }
            i += 1;
        }
        let keep = buf.len().min(64);
        self.carry = buf[buf.len() - keep..].to_vec();
    }
}

impl<S: Read> Read for Sniff<S> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(out)?;
        if n > 0 {
            self.scan(&out[..n]);
        }
        Ok(n)
    }
}

impl<S: Seek> Seek for Sniff<S> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        // Bytes on either side of a seek aren't contiguous.
        self.carry.clear();
        self.inner.seek(pos)
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
    /// to `start_frames` into the real audio.
    pub fn new(delay: u64, total: u64, seeked: bool, start_frames: u64) -> Self {
        if seeked {
            // The seek lands on the raw timeline; just keep the end right.
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

    #[test]
    fn finds_edit_list_across_reads() {
        // mvhd v0: version/flags, ctime, mtime, timescale 1000, ...
        let mut mvhd = vec![0, 0, 0, 0];
        mvhd.extend_from_slice(&0u32.to_be_bytes());
        mvhd.extend_from_slice(&0u32.to_be_bytes());
        mvhd.extend_from_slice(&1000u32.to_be_bytes());
        mvhd.extend_from_slice(&[0; 80]);
        // elst v0: 1 entry, duration 247973 ms, media time 2048, rate 1.0
        let mut elst = vec![0, 0, 0, 0];
        elst.extend_from_slice(&1u32.to_be_bytes());
        elst.extend_from_slice(&247_973u32.to_be_bytes());
        elst.extend_from_slice(&2048i32.to_be_bytes());
        elst.extend_from_slice(&[0, 1, 0, 0]);
        let mut file = vec![0u8; 100];
        file.extend(atom(b"mvhd", &mvhd));
        file.extend(atom(b"elst", &elst));
        file.extend(vec![0u8; 50]);
        let (mut s, found) = Sniff::new(std::io::Cursor::new(file));
        let mut out = [0u8; 7]; // tiny reads: atoms straddle them
        while s.read(&mut out).unwrap() > 0 {}
        let f = *found.lock().unwrap();
        assert_eq!(f.trims(44_100), Some((2048, (247_973 * 44_100 + 500) / 1000)));
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
        let (sniff, found) = super::Sniff::new(file);
        let mss = MediaSourceStream::new(Box::new(sniff), Default::default());
        let mut hint = symphonia::core::formats::probe::Hint::new();
        hint.with_extension("m4a");
        let mut fmt = symphonia::default::get_probe().probe(&hint, mss, FormatOptions::default(), MetadataOptions::default()).unwrap();
        let track = fmt.default_track(TrackType::Audio).unwrap().clone();
        let params = track.codec_params.as_ref().unwrap().audio().unwrap().clone();
        let rate = params.sample_rate.unwrap();
        let (delay, total) = found.lock().unwrap().trims(rate).expect("edit list");
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
