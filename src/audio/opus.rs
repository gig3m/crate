//! An Opus decoder for symphonia, backed by the system libopus.
//!
//! Remote playback asks Plex for Opus in Ogg: symphonia's Ogg reader turns
//! the stream's pre-skip and final granule into each packet's `trim_start`
//! / `trim_end`, and this decoder drops those frames, so consecutive tracks
//! join sample-accurately (gapless). Symphonia has no Opus decoder of its
//! own.

use std::os::raw::{c_int, c_uchar};

use symphonia::core::audio::{AsGenericAudioBufferRef, AudioBuffer, AudioMut, AudioSpec, Channels, GenericAudioBufferRef, Position};
use symphonia::core::codecs::CodecInfo;
use symphonia::core::codecs::audio::well_known::CODEC_ID_OPUS;
use symphonia::core::codecs::audio::{AudioCodecParameters, AudioDecoder, AudioDecoderOptions, FinalizeResult};
use symphonia::core::codecs::registry::{RegisterableAudioDecoder, SupportedAudioCodec};
use symphonia::core::errors::{Result, decode_error, unsupported_error};
use symphonia::core::packet::PacketRef;
use symphonia::core::support_audio_codec;

#[repr(C)]
struct OpusDecoderC {
    _private: [u8; 0],
}

#[link(name = "opus")]
unsafe extern "C" {
    fn opus_decoder_create(fs: i32, channels: c_int, error: *mut c_int) -> *mut OpusDecoderC;
    fn opus_decode_float(st: *mut OpusDecoderC, data: *const c_uchar, len: i32, pcm: *mut f32, frame_size: c_int, decode_fec: c_int) -> c_int;
    fn opus_decoder_ctl(st: *mut OpusDecoderC, request: c_int, ...) -> c_int;
    fn opus_decoder_destroy(st: *mut OpusDecoderC);
}

const OPUS_RESET_STATE: c_int = 4028;
const OPUS_SET_GAIN: c_int = 4034;
/// Opus always decodes at 48 kHz here.
const RATE: u32 = 48_000;
/// Longest Opus packet: 120 ms.
const MAX_FRAMES: usize = 5760;

pub struct OpusDecoder {
    st: *mut OpusDecoderC,
    params: AudioCodecParameters,
    opts: AudioDecoderOptions,
    channels: usize,
    /// Pre-skip frames still to drop: the encoder's delay at the stream's
    /// start (symphonia's Ogg reader records it but doesn't trim it).
    skip: usize,
    pcm: Vec<f32>,
    buf: AudioBuffer<f32>,
}

// The libopus state is only touched through &mut self.
unsafe impl Send for OpusDecoder {}
unsafe impl Sync for OpusDecoder {}

impl Drop for OpusDecoder {
    fn drop(&mut self) {
        unsafe { opus_decoder_destroy(self.st) }
    }
}

impl OpusDecoder {
    pub fn try_new(params: &AudioCodecParameters, opts: &AudioDecoderOptions) -> Result<Self> {
        // OpusHead (RFC 7845 §5.1): "OpusHead", version, channels, pre-skip,
        // input rate, output gain (Q7.8 dB), mapping family.
        let head = params.extra_data.as_deref().unwrap_or_default();
        if head.len() < 19 || &head[..8] != b"OpusHead" {
            return unsupported_error("opus: no OpusHead");
        }
        let channels = head[9] as usize;
        let pre_skip = u16::from_le_bytes([head[10], head[11]]) as usize;
        let gain = i16::from_le_bytes([head[16], head[17]]);
        let family = head[18];
        if family != 0 || !(1..=2).contains(&channels) {
            return unsupported_error("opus: only mono/stereo (mapping family 0)");
        }
        let mut err: c_int = 0;
        let st = unsafe { opus_decoder_create(RATE as i32, channels as c_int, &mut err) };
        if st.is_null() || err != 0 {
            return decode_error("opus: decoder create failed");
        }
        if gain != 0 {
            unsafe { opus_decoder_ctl(st, OPUS_SET_GAIN, gain as c_int) };
        }
        let layout = if channels == 1 { Position::FRONT_LEFT } else { Position::FRONT_LEFT | Position::FRONT_RIGHT };
        let spec = AudioSpec::new(RATE, Channels::Positioned(layout));
        Ok(Self { st, params: params.clone(), opts: *opts, channels, skip: if opts.gapless { pre_skip } else { 0 }, pcm: vec![0.0; MAX_FRAMES * channels], buf: AudioBuffer::new(spec, MAX_FRAMES) })
    }

    fn decode_inner(&mut self, packet: &PacketRef<'_>) -> Result<()> {
        let data = packet.data;
        let n = unsafe { opus_decode_float(self.st, data.as_ptr(), data.len() as i32, self.pcm.as_mut_ptr(), MAX_FRAMES as c_int, 0) };
        if n < 0 {
            return decode_error("opus: corrupt packet");
        }
        let n = n as usize;
        self.buf.clear();
        self.buf.render_uninit(Some(n));
        for ch in 0..self.channels {
            let Some(plane) = self.buf.plane_mut(ch) else { return decode_error("opus: missing plane") };
            for (i, s) in plane.iter_mut().enumerate().take(n) {
                *s = self.pcm[i * self.channels + ch];
            }
        }
        if self.opts.gapless {
            let front = (packet.trim_start.get() as usize).max(self.skip.min(n));
            self.skip -= self.skip.min(n);
            self.buf.trim(front.min(n), (packet.trim_end.get() as usize).min(n - front.min(n)));
        }
        Ok(())
    }
}

impl AudioDecoder for OpusDecoder {
    fn reset(&mut self) {
        // A reset is a seek: the stream's start (and its pre-skip) is behind us.
        unsafe { opus_decoder_ctl(self.st, OPUS_RESET_STATE) };
        self.skip = 0;
    }

    fn codec_info(&self) -> &CodecInfo {
        &Self::supported_codecs().first().expect("one codec").info
    }

    fn codec_params(&self) -> &AudioCodecParameters {
        &self.params
    }

    fn decode_ref(&mut self, packet: &PacketRef<'_>) -> Result<GenericAudioBufferRef<'_>> {
        if let Err(e) = self.decode_inner(packet) {
            self.buf.clear();
            return Err(e);
        }
        Ok(self.buf.as_generic_audio_buffer_ref())
    }

    fn finalize(&mut self) -> FinalizeResult {
        Default::default()
    }

    fn last_decoded(&self) -> GenericAudioBufferRef<'_> {
        self.buf.as_generic_audio_buffer_ref()
    }
}

impl RegisterableAudioDecoder for OpusDecoder {
    fn try_registry_new(params: &AudioCodecParameters, opts: &AudioDecoderOptions) -> Result<Box<dyn AudioDecoder>>
    where
        Self: Sized,
    {
        Ok(Box::new(Self::try_new(params, opts)?))
    }

    fn supported_codecs() -> &'static [SupportedAudioCodec] {
        &[support_audio_codec!(CODEC_ID_OPUS, "opus", "Opus (libopus)")]
    }
}

/// Symphonia's codecs plus Opus.
pub fn codecs() -> &'static symphonia::core::codecs::registry::CodecRegistry {
    static REG: std::sync::OnceLock<symphonia::core::codecs::registry::CodecRegistry> = std::sync::OnceLock::new();
    REG.get_or_init(|| {
        let mut r = symphonia::core::codecs::registry::CodecRegistry::new();
        symphonia::default::register_enabled_codecs(&mut r);
        r.register_audio_decoder::<OpusDecoder>();
        r
    })
}

#[cfg(test)]
mod tests {
    /// Decode an Ogg Opus file end to end (CRATE_OPUS_SAMPLE=path; skipped
    /// without it) and print the frame count, pre-skip and end trim applied.
    #[test]
    fn decodes_ogg_opus_sample() {
        use symphonia::core::codecs::audio::AudioDecoderOptions;
        use symphonia::core::formats::{FormatOptions, TrackType};
        use symphonia::core::io::MediaSourceStream;
        use symphonia::core::meta::MetadataOptions;
        let Some(path) = std::env::var_os("CRATE_OPUS_SAMPLE") else { return };
        let mss = MediaSourceStream::new(Box::new(std::fs::File::open(path).unwrap()), Default::default());
        let mut hint = symphonia::core::formats::probe::Hint::new();
        hint.with_extension("ogg");
        let mut fmt = symphonia::default::get_probe().probe(&hint, mss, FormatOptions::default(), MetadataOptions::default()).unwrap();
        let track = fmt.default_track(TrackType::Audio).unwrap().clone();
        let params = track.codec_params.as_ref().unwrap().audio().unwrap().clone();
        let mut dec = super::codecs().make_audio_decoder(&params, &AudioDecoderOptions::default().gapless(true)).unwrap();
        let (mut frames, mut first) = (0usize, None);
        let mut out = Vec::new();
        loop {
            let p = match fmt.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => break,
                // Plex's Ogg has no end-of-stream flag: symphonia ends it this way
                // (the engine also checks the known length).
                Err(symphonia::core::errors::Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => panic!("next_packet after {frames} frames: {e}"),
            };
            let b = dec.decode(&p).unwrap();
            let n = b.frames();
            if first.is_none() {
                first = Some((p.trim_start.get(), n));
            }
            let mut tmp = vec![0f32; n * 2];
            b.copy_to_slice_interleaved(&mut tmp[..]);
            out.extend(tmp);
            frames += n;
        }
        eprintln!("frames {frames} ({:.4} s); first packet trim_start/frames {first:?}", frames as f64 / 48_000.0);
        if let Some(dump) = std::env::var_os("CRATE_OPUS_DUMP") {
            std::fs::write(dump, out.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>()).unwrap();
        }
        assert!(frames > 0);
        if let Some(want) = std::env::var("CRATE_OPUS_FRAMES").ok().and_then(|v| v.parse::<usize>().ok()) {
            assert_eq!(frames, want);
        }
    }
}
