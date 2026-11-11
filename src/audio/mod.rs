//! Crate audio engine (spike quality, shaped for the real app).
//!
//! Threads:
//! - decoder: pops the shared queue, decodes with symphonia, pushes f32 frames into
//!   an rtrb ring. Asks the PipeWire thread to rebuild the stream when the
//!   sample rate changes between tracks.
//! - pipewire: owns the main loop and the stream. The process callback (RT)
//!   only copies from the ring, counts frames and samples the graph rate.
//! - caller: reads `Stats` and receives `Event`s.

pub mod eq;
pub mod source;

use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use pipewire as pw;
use pw::{properties::properties, spa};
use spa::{pod::Pod, sys as spa_sys};
use symphonia::core::{
    codecs::{CodecParameters, audio::AudioDecoderOptions},
    formats::{FormatOptions, TrackType, probe::Hint},
    io::MediaSourceStream,
    meta::MetadataOptions,
};

pub use source::Source;

/// A queued track: where to read it, and the linear gain the decoder applies
/// to its samples (loudness leveling). 1.0 leaves the samples untouched.
#[derive(Clone, Debug)]
pub struct Item {
    pub src: Source,
    pub gain: f32,
    /// Crossfade from the previous track into this one, in ms (0 = gapless).
    /// Only applied when both run at the same sample rate.
    pub fade_ms: u32,
}

impl From<Source> for Item {
    fn from(src: Source) -> Self {
        Item { src, gain: 1.0, fade_ms: 0 }
    }
}

/// Where decoded samples go: the ring, plus the counters and stream
/// activation that every push updates.
struct Out<'a> {
    prod: &'a mut rtrb::Producer<f32>,
    cap: usize,
    activated: &'a mut bool,
    queued_frames: &'a mut u64,
    queued_us: &'a mut u64,
    stream_frames: &'a mut u64,
}

impl Out<'_> {
    /// Push `data` (interleaved) at `rate`. False if the engine was stopped.
    fn push(&mut self, data: &[f32], rate: u32, cmd_tx: &pw::channel::Sender<Cmd>, stats: &Stats) -> bool {
        let mut off = 0;
        while off < data.len() {
            let free = self.prod.slots();
            if free == 0 {
                // A full ring on a stream that hasn't started can't drain: start it.
                if !*self.activated {
                    let _ = cmd_tx.send(Cmd::Activate);
                    *self.activated = true;
                }
                if stats.stop.load(Ordering::Acquire) {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            let take = free.min(data.len() - off);
            self.prod.write_chunk_uninit(take).unwrap().fill_from_iter(data[off..off + take].iter().copied());
            off += take;
        }
        let prefill = (rate as usize / 10) * CHANNELS; // 100 ms
        if !*self.activated && self.cap - self.prod.slots() >= prefill {
            *self.activated = true;
            let _ = cmd_tx.send(Cmd::Activate);
        }
        let frames = (data.len() / CHANNELS) as u64;
        *self.queued_frames += frames;
        *self.stream_frames += frames;
        *self.queued_us += frames * 1_000_000 / rate.max(1) as u64;
        true
    }
}

/// Bound a crossfade sum under full scale without touching what can't clip.
/// With inputs within ±1, |x| ≤ m = out_g + in_g (1 at the fade's edges,
/// √2 mid-fade). Samples under 2 − m pass untouched; [2 − m, m] bends
/// smoothly into [2 − m, 1). At the edges (m = 1) it's the identity, so the
/// fade joins the unfaded audio with no step.
fn fade_limit(x: f32, m: f32) -> f32 {
    let span = m - 1.0;
    let k = 1.0 - span;
    let a = x.abs();
    if span <= 1e-6 || a <= k { x } else { x.signum() * (k + span * ((a - k) / span).tanh()) }
}

/// Equal-power crossfade of `from` (fading out) into `into` (fading in), in
/// place in `into`, starting `done` frames into a `total`-frame fade.
/// Returns frames mixed.
fn mix_fade(into: &mut [f32], from: &mut VecDeque<f32>, done: usize, total: usize) -> usize {
    let mut frames = 0;
    for fr in into.as_chunks_mut::<CHANNELS>().0 {
        if from.len() < CHANNELS {
            break;
        }
        let t = ((done + frames) as f32 + 0.5) / total.max(1) as f32;
        let (out_g, in_g) = ((t * std::f32::consts::FRAC_PI_2).cos(), (t * std::f32::consts::FRAC_PI_2).sin());
        let m = out_g + in_g;
        for s in fr.iter_mut() {
            let a = from.pop_front().unwrap_or(0.0);
            *s = fade_limit(a * out_g + *s * in_g, m).clamp(-1.0, 1.0);
        }
        frames += 1;
    }
    frames
}

pub const CHANNELS: usize = 2;

#[derive(Default)]
pub struct Stats {
    /// Played time in microseconds (exact across rate changes).
    pub played_us: AtomicU64,
    pub frames_played: AtomicU64,
    pub underrun_callbacks: AtomicU64,
    pub callbacks: AtomicU64,
    pub max_cb_ns: AtomicU64,
    /// Graph clock rate seen by the stream (Hz), sampled every cycle.
    pub graph_rate: AtomicU32,
    pub stream_rate: AtomicU32,
    /// Set by the RT thread when it writes real audio after a (re)connect.
    pub got_data: AtomicBool,
    pub decoder_done: AtomicBool,
    /// Set by Engine::stop / Drop; decoder and PipeWire threads wind down.
    pub stop: Arc<AtomicBool>,
    /// Ring is being emptied on purpose ahead of a stream rebuild.
    pub rebuilding: AtomicBool,
    /// The caller asked for pause; activation never overrides it.
    pub paused: AtomicBool,
    /// Set by the drained callback when a non-final drain completes.
    pub drained: AtomicBool,
    /// The pending drain is the end of the queue (quit after it).
    pub final_drain: AtomicBool,
    /// Volume as attenuation in millionths (0 = full volume, the default):
    /// applied as PipeWire's stream channelVolumes, re-applied on rebuilds.
    pub attenuation: AtomicU32,
    /// Ten-band EQ: gains (f32 bits, dB), linear preamp (f32 bits), and a
    /// generation the RT thread watches to recompute its filters.
    pub eq_gains: [AtomicU32; 10],
    pub eq_preamp: AtomicU32,
    pub eq_gen: AtomicU64,
}

impl Stats {
    /// Linear stream volume, 0.0..=1.0.
    pub fn volume(&self) -> f32 {
        1.0 - self.attenuation.load(Ordering::Acquire) as f32 / 1_000_000.0
    }
}

#[derive(Debug)]
pub enum Event {
    /// `at_us`: where this track begins on the played-time clock (`Stats::played_us`).
    /// `gain` is the leveling factor the decoder applies to this track's samples.
    TrackStarted { index: usize, label: String, source_rate: u32, bits: u32, at_frame: u64, at_us: u64, gain: f32 },
    Reconnected { from: u32, to: u32, silent_ms: f64 },
    FirstAudio { ms_since_play: f64 },
    Finished,
    Error(String),
}

enum Cmd {
    /// Rebuild the stream at `rate`, created inactive.
    Configure { rate: u32, ack: mpsc::Sender<()> },
    /// The ring holds enough audio to start; actually starts unless paused.
    Activate,
    /// Caller pause (false) / resume (true).
    SetActive(bool),
    /// Stream volume changed (read from Stats::attenuation).
    Volume,
    /// Play out what the stream has queued. `last`: quit afterwards.
    Drain { last: bool },
    /// End without draining (the stream never received audio).
    Finish,
    Stop,
}

/// Tracks the decoder hasn't opened yet. The decoder pops from the front;
/// the caller can append (radio refills) or replace what's left (shuffle).
struct Queue {
    items: VecDeque<Item>,
    /// Tracks the decoder has opened (= the next track's engine index).
    started: usize,
    /// The decoder reached the end and is winding down: no more changes.
    closed: bool,
}

pub struct Engine {
    pub stats: Arc<Stats>,
    pub events: mpsc::Receiver<Event>,
    cmd: pw::channel::Sender<Cmd>,
    queue: Arc<Mutex<Queue>>,
}

impl Engine {
    /// Pause (false) or resume (true) without losing position.
    pub fn set_active(&self, active: bool) {
        let _ = self.cmd.send(Cmd::SetActive(active));
    }

    pub fn stop(&self) {
        self.stats.stop.store(true, Ordering::Release);
        let _ = self.cmd.send(Cmd::Stop);
    }

    /// Set the ten-band EQ (dB per band; all zero = bypassed, bit-perfect).
    /// Takes effect on the next audio cycle. Single writer: call it from one
    /// thread (the app's UI thread); the RT reader's seqlock assumes that.
    pub fn set_eq(&self, gains: [f32; 10]) {
        let gains = gains.map(|g| if g.is_finite() { g.clamp(-12.0, 12.0) } else { 0.0 });
        let pre = eq::preamp(&gains);
        // Seqlock (one writer, the UI thread): odd while writing, even when done.
        self.stats.eq_gen.fetch_add(1, Ordering::Relaxed);
        std::sync::atomic::fence(Ordering::Release);
        for (slot, g) in self.stats.eq_gains.iter().zip(gains) {
            slot.store(g.to_bits(), Ordering::Relaxed);
        }
        self.stats.eq_preamp.store(pre.to_bits(), Ordering::Relaxed);
        self.stats.eq_gen.fetch_add(1, Ordering::Release);
    }

    /// Linear volume 0.0..=1.0 (PipeWire software volume; 1.0 leaves the
    /// samples untouched, anything lower is no longer bit-perfect).
    pub fn set_volume(&self, linear: f32) {
        let att = ((1.0 - linear.clamp(0.0, 1.0)) * 1_000_000.0).round() as u32;
        self.stats.attenuation.store(att, Ordering::Release);
        let _ = self.cmd.send(Cmd::Volume);
    }

    /// Add tracks to the end. False once the decoder has finished the queue
    /// (the caller must start a new engine for them).
    pub fn append(&self, more: Vec<Item>) -> bool {
        let mut q = self.queue.lock().unwrap();
        if q.closed {
            return false;
        }
        q.items.extend(more);
        true
    }

    /// The decoder reached the end of its queue: it takes no more tracks.
    pub fn is_closed(&self) -> bool {
        self.queue.lock().unwrap().closed
    }

    /// How many tracks the decoder has opened so far.
    pub fn started(&self) -> usize {
        self.queue.lock().unwrap().started
    }

    /// Replace the tracks not opened yet, but only if the decoder has opened
    /// exactly `started` tracks (so the caller's view of what's next is
    /// current). False means retry with a fresh `started()`, or the queue closed.
    pub fn replace_upcoming(&self, started: usize, upcoming: Vec<Item>) -> bool {
        let mut q = self.queue.lock().unwrap();
        if q.closed || q.started != started {
            return false;
        }
        q.items = upcoming.into();
        true
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Engine {
    /// Start playing `queue` on `target` (a sink node.name; None = default).
    pub fn play(queue: Vec<Item>, target: Option<String>) -> anyhow::Result<Self> {
        Self::play_from(queue, 0, target)
    }

    /// Like `play`, starting `start_ms` into the first track.
    pub fn play_from(queue: Vec<Item>, start_ms: u64, target: Option<String>) -> anyhow::Result<Self> {
        let stats = Arc::new(Stats::default());
        let queue = Arc::new(Mutex::new(Queue { items: queue.into(), started: 0, closed: false }));
        let (ev_tx, events) = mpsc::channel();
        let (cmd_tx, cmd_rx) = pw::channel::channel::<Cmd>();

        // 2 s at 192 kHz stereo; reused across rate changes.
        let (prod, cons) = rtrb::RingBuffer::<f32>::new(192_000 * CHANNELS * 2);
        let cons = Arc::new(Mutex::new(cons));

        {
            let (stats, ev_tx, cons) = (stats.clone(), ev_tx.clone(), cons.clone());
            std::thread::Builder::new()
                .name("pipewire".into())
                .spawn(move || {
                    if let Err(e) = pw_thread(cmd_rx, cons, stats, target, ev_tx.clone()) {
                        let _ = ev_tx.send(Event::Error(format!("pipewire: {e:#}")));
                    }
                })?;
        }
        let cmd = cmd_tx.clone();
        {
            let (stats, ev_tx, queue) = (stats.clone(), ev_tx.clone(), queue.clone());
            std::thread::Builder::new()
                .name("decoder".into())
                .spawn(move || {
                    let stop_tx = cmd_tx.clone();
                    let res = decoder_thread(queue.clone(), start_ms, prod, cmd_tx, stats.clone(), ev_tx.clone());
                    // Whatever ended the decoder, appends no longer have anyone to play them.
                    queue.lock().unwrap().closed = true;
                    if let Err(e) = res {
                        let _ = ev_tx.send(Event::Error(format!("decoder: {e:#}")));
                        // A fatal error ends the pipeline; nothing else would stop PipeWire.
                        stats.stop.store(true, Ordering::Release);
                        let _ = stop_tx.send(Cmd::Stop);
                    }
                    stats.decoder_done.store(true, Ordering::Release);
                })?;
        }
        Ok(Self { stats, events, cmd, queue })
    }
}

/// Sleep-poll until `cond` holds. Returns false if the engine was stopped first.
fn wait_for(stats: &Stats, mut cond: impl FnMut() -> bool) -> bool {
    loop {
        if stats.stop.load(Ordering::Acquire) {
            return false;
        }
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn decoder_thread(
    queue: Arc<Mutex<Queue>>,
    start_ms: u64,
    mut prod: rtrb::Producer<f32>,
    cmd_tx: pw::channel::Sender<Cmd>,
    stats: Arc<Stats>,
    ev: mpsc::Sender<Event>,
) -> anyhow::Result<()> {
    let play_started = Instant::now();
    let cap = prod.buffer().capacity();
    let mut cur_rate = 0u32;
    // Whether Activate has been sent for the current stream (persists across
    // same-rate tracks, so short tracks never leave the stream inactive).
    let mut activated = false;
    let mut queued_frames = 0u64;
    let mut queued_us = 0u64;
    let mut scratch: Vec<f32> = Vec::new();
    // Crossfades: the current track's held-back end (only while the next
    // queued track will fade in), and the previous track's end being mixed
    // into the current one's start.
    let mut tail: VecDeque<f32> = VecDeque::new();
    let mut tail_rate = 0u32;
    // Frames the track that left `tail` played (its whole length).
    let mut tail_track_frames = 0usize;
    let mut fading: VecDeque<f32> = VecDeque::new();
    let mut fade_total = 0usize;
    let mut fade_done = 0usize;
    let mut drained: Vec<f32> = Vec::new();
    let mut first_audio_reported = false;
    let mut pending_reconnect: Option<(u32, u32, Instant)> = None;
    // Frames pushed since the current stream was built: a stream that never
    // got audio has nothing to drain (and an inactive one never would).
    let mut stream_frames = 0u64;
    // Spike/testing aid: cap each track at N seconds.
    let track_limit: Option<f64> = std::env::var("CRATE_TRACK_LIMIT").ok().and_then(|v| v.parse().ok());

    loop {
        let popped = {
            let mut q = queue.lock().unwrap();
            q.items.pop_front().map(|src| {
                q.started += 1;
                (q.started - 1, src)
            })
        };
        let (index, item) = match popped {
            Some(next) => next,
            None => {
                // Nothing to fade into: the held-back end plays as is.
                if !tail.is_empty() {
                    drained.clear();
                    drained.extend(tail.drain(..));
                    let mut out = Out { prod: &mut prod, cap, activated: &mut activated, queued_frames: &mut queued_frames, queued_us: &mut queued_us, stream_frames: &mut stream_frames };
                    if !out.push(&drained, tail_rate, &cmd_tx, &stats) {
                        return Ok(());
                    }
                }
                // Out of tracks for now. Let what's buffered play while an
                // append (radio refill) may still arrive; once the ring is
                // empty, close the queue and finish.
                if !activated && stream_frames > 0 {
                    let _ = cmd_tx.send(Cmd::Activate);
                    activated = true;
                }
                let mut next = None;
                loop {
                    if stats.stop.load(Ordering::Acquire) {
                        return Ok(());
                    }
                    {
                        let mut q = queue.lock().unwrap();
                        if let Some(src) = q.items.pop_front() {
                            q.started += 1;
                            next = Some((q.started - 1, src));
                            break;
                        }
                        if prod.slots() >= cap {
                            q.closed = true;
                            break;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                match next {
                    Some(n) => n,
                    None => break,
                }
            }
        };
        let (src, gain) = (item.src, item.gain);
        // A gain that isn't a sane finite factor is ignored rather than trusted.
        let gain = if gain.is_finite() && gain > 0.0 && gain <= 4.0 { gain } else { 1.0 };
        let mss = MediaSourceStream::new(src.open(stats.stop.clone())?, Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = src.ext() {
            hint.with_extension(&ext);
        }
        let mut fmt = symphonia::default::get_probe().probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )?;
        let track = fmt
            .default_track(TrackType::Audio)
            .ok_or_else(|| anyhow::anyhow!("no audio track"))?
            .clone();
        let Some(CodecParameters::Audio(params)) = &track.codec_params else {
            anyhow::bail!("not audio")
        };
        let rate = params.sample_rate.unwrap_or(48_000);
        let bits = params.bits_per_sample.unwrap_or(0);

        // Crossfade into this track only at the same rate, with a held-back
        // end to fade from, and not when starting mid-track (a seek).
        let fade_frames = (item.fade_ms as u64 * rate as u64 / 1000) as usize;
        // The track must be known to outlast the fade twice over, or the fade
        // would run past its end (into the next track); unknown length: gapless.
        let num_frames = src.len_ms().map(|ms| ms * rate as u64 / 1000).or(track.num_frames);
        let long_enough = num_frames.is_some_and(|n| n >= 2 * fade_frames as u64);
        if fade_frames > 0 && !long_enough && num_frames.is_none() {
            eprintln!("[engine] no fade into {}: its length isn't known", src.label());
        }
        let crossfade = fade_frames > 0 && long_enough && rate == tail_rate && tail.len() >= CHANNELS && !(index == 0 && start_ms > 0);
        if crossfade {
            // Fade over the shorter of what's held and what this track asks
            // for, and at most half the outgoing track, so a short one is
            // still heard at full level before it fades.
            let keep = tail.len().min(fade_frames * CHANNELS).min(tail_track_frames / 2 * CHANNELS).max(CHANNELS);
            let lead = tail.len() - keep;
            if lead > 0 {
                drained.clear();
                drained.extend(tail.drain(..lead));
                let mut out = Out { prod: &mut prod, cap, activated: &mut activated, queued_frames: &mut queued_frames, queued_us: &mut queued_us, stream_frames: &mut stream_frames };
                if !out.push(&drained, tail_rate, &cmd_tx, &stats) {
                    return Ok(());
                }
            }
            fading = std::mem::take(&mut tail);
            fade_total = fading.len() / CHANNELS;
            fade_done = 0;
        } else if !tail.is_empty() {
            // Gapless (or a rate change ahead): the held-back end plays as is.
            drained.clear();
            drained.extend(tail.drain(..));
            let mut out = Out { prod: &mut prod, cap, activated: &mut activated, queued_frames: &mut queued_frames, queued_us: &mut queued_us, stream_frames: &mut stream_frames };
            if !out.push(&drained, tail_rate, &cmd_tx, &stats) {
                return Ok(());
            }
        }

        if rate != cur_rate {
            stats.rebuilding.store(true, Ordering::Release);
            if cur_rate != 0 {
                // Start the old stream if a short track never reached prefill,
                // let the ring empty, then drain PipeWire so the tail plays.
                if !activated && prod.slots() < cap {
                    let _ = cmd_tx.send(Cmd::Activate);
                }
                if !wait_for(&stats, || prod.slots() >= cap) {
                    return Ok(());
                }
                if stream_frames > 0 {
                    stats.drained.store(false, Ordering::Release);
                    let _ = cmd_tx.send(Cmd::Drain { last: false });
                    if !wait_for(&stats, || stats.drained.load(Ordering::Acquire)) {
                        return Ok(());
                    }
                }
            }
            let t_empty = Instant::now();
            stats.got_data.store(false, Ordering::Release);
            let (ack_tx, ack_rx) = mpsc::channel();
            if cmd_tx.send(Cmd::Configure { rate, ack: ack_tx }).is_err() {
                anyhow::bail!("pipewire thread gone");
            }
            // Bounded, stop-aware: the pipewire channel accepts sends even
            // after its receiver is gone, so a plain recv() could hang forever.
            loop {
                match ack_rx.recv_timeout(Duration::from_millis(20)) {
                    Ok(()) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) if !stats.stop.load(Ordering::Acquire) => {}
                    _ => return Ok(()),
                }
            }
            stats.rebuilding.store(false, Ordering::Release);
            activated = false;
            stream_frames = 0;
            if cur_rate != 0 {
                pending_reconnect = Some((cur_rate, rate, t_empty));
            }
            cur_rate = rate;
        }
        let _ = ev.send(Event::TrackStarted {
            index,
            label: src.label(),
            source_rate: rate,
            bits,
            at_frame: queued_frames,
            at_us: queued_us,
            gain,
        });
        if index == 0 && start_ms > 0 {
            use symphonia::core::formats::{SeekMode, SeekTo};
            use symphonia::core::units::Time;
            let time = Time::try_from_secs_f64(start_ms as f64 / 1000.0).unwrap_or_default();
            fmt.seek(SeekMode::Accurate, SeekTo::Time { time, track_id: Some(track.id) })?;
        }

        // Hold back this track's end only if the next queued track will fade
        // in (checked now; if the queue changes, the end just plays as is).
        let hold = {
            let q = queue.lock().unwrap();
            q.items.front().map(|n| (n.fade_ms.min(12_000) as u64 * rate as u64 / 1000) as usize * CHANNELS).unwrap_or(0)
        };
        tail_rate = rate;
        let mut dec = symphonia::default::get_codecs()
            .make_audio_decoder(params, &AudioDecoderOptions::default().gapless(true))?;
        let limit_frames = track_limit.map(|s| (s * rate as f64) as u64);
        let mut track_frames = 0u64;
        while let Some(packet) = fmt.next_packet()? {
            if stats.stop.load(Ordering::Acquire) {
                return Ok(());
            }
            if limit_frames.is_some_and(|l| track_frames >= l) {
                break;
            }
            if packet.track_id != track.id {
                continue;
            }
            let buf = dec.decode(&packet)?;
            let ch = buf.spec().channels().count();
            anyhow::ensure!(ch == CHANNELS, "only stereo in the spike (got {ch})");
            scratch.resize(buf.frames() * ch, 0.0);
            buf.copy_to_slice_interleaved(&mut scratch[..]);
            // Loudness leveling. Exactly 1.0 skips the loop, so leveling off
            // stays bit-perfect.
            if gain != 1.0 {
                for s in scratch.iter_mut() {
                    *s = (*s * gain).clamp(-1.0, 1.0);
                }
            }
            track_frames += (scratch.len() / CHANNELS) as u64;
            // The previous track's end fades out under this one's start.
            if !fading.is_empty() {
                fade_done += mix_fade(&mut scratch, &mut fading, fade_done, fade_total);
            }
            let mut out = Out { prod: &mut prod, cap, activated: &mut activated, queued_frames: &mut queued_frames, queued_us: &mut queued_us, stream_frames: &mut stream_frames };
            if hold == 0 {
                if !out.push(&scratch, rate, &cmd_tx, &stats) {
                    return Ok(());
                }
            } else {
                tail.extend(scratch.iter().copied());
                if tail.len() > hold {
                    let excess = tail.len() - hold;
                    drained.clear();
                    drained.extend(tail.drain(..excess));
                    if !out.push(&drained, rate, &cmd_tx, &stats) {
                        return Ok(());
                    }
                }
            }
            if stats.got_data.load(Ordering::Acquire) {
                if !first_audio_reported {
                    first_audio_reported = true;
                    let _ = ev.send(Event::FirstAudio {
                        ms_since_play: play_started.elapsed().as_secs_f64() * 1000.0,
                    });
                }
                if let Some((from, to, t)) = pending_reconnect.take() {
                    let _ = ev.send(Event::Reconnected { from, to, silent_ms: t.elapsed().as_secs_f64() * 1000.0 });
                }
            }
        }
        tail_track_frames = track_frames as usize;
        // Safety net (a track shorter than its reported length, or the
        // CRATE_TRACK_LIMIT test cap): the rest
        // of the previous end fades out on its own, in time order, after this
        // track's samples. They may still be held in `tail`, so it goes there.
        if !fading.is_empty() {
            drained.clear();
            let rest = fading.len() / CHANNELS;
            drained.resize(rest * CHANNELS, 0.0);
            mix_fade(&mut drained, &mut fading, fade_done, fade_total);
            fading.clear();
            if hold > 0 || !tail.is_empty() {
                tail.extend(drained.iter().copied());
            } else {
                let mut out = Out { prod: &mut prod, cap, activated: &mut activated, queued_frames: &mut queued_frames, queued_us: &mut queued_us, stream_frames: &mut stream_frames };
                if !out.push(&drained, rate, &cmd_tx, &stats) {
                    return Ok(());
                }
            }
        }
    }
    // Nothing follows: the held-back end plays as is.
    if !tail.is_empty() {
        drained.clear();
        drained.extend(tail.drain(..));
        let mut out = Out { prod: &mut prod, cap, activated: &mut activated, queued_frames: &mut queued_frames, queued_us: &mut queued_us, stream_frames: &mut stream_frames };
        if !out.push(&drained, tail_rate, &cmd_tx, &stats) {
            return Ok(());
        }
    }
    if !activated {
        let _ = cmd_tx.send(Cmd::Activate);
    }
    // Nothing more is coming: an empty ring from here on is the end, not an underrun.
    stats.decoder_done.store(true, Ordering::Release);
    if !wait_for(&stats, || prod.slots() >= cap) {
        return Ok(());
    }
    let _ = cmd_tx.send(if stream_frames > 0 { Cmd::Drain { last: true } } else { Cmd::Finish });
    Ok(())
}

fn format_param(rate: u32) -> Vec<u8> {
    let mut info = spa::param::audio::AudioInfoRaw::new();
    info.set_format(spa::param::audio::AudioFormat::F32LE);
    info.set_rate(rate);
    info.set_channels(CHANNELS as u32);
    let mut position = [0; spa::param::audio::MAX_CHANNELS];
    position[0] = spa_sys::SPA_AUDIO_CHANNEL_FL;
    position[1] = spa_sys::SPA_AUDIO_CHANNEL_FR;
    info.set_position(position);
    pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pw::spa::pod::Value::Object(pw::spa::pod::Object {
            type_: spa_sys::SPA_TYPE_OBJECT_Format,
            id: spa_sys::SPA_PARAM_EnumFormat,
            properties: info.into(),
        }),
    )
    .unwrap()
    .0
    .into_inner()
}

struct Rt {
    cons: Arc<Mutex<rtrb::Consumer<f32>>>,
    stats: Arc<Stats>,
    eq: eq::Eq,
}

type Live = (pw::stream::StreamBox<'static>, pw::stream::StreamListener<Rt>);

fn pw_thread(
    cmd_rx: pw::channel::Receiver<Cmd>,
    cons: Arc<Mutex<rtrb::Consumer<f32>>>,
    stats: Arc<Stats>,
    target: Option<String>,
    ev: mpsc::Sender<Event>,
) -> anyhow::Result<()> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;

    let live: std::rc::Rc<std::cell::RefCell<Option<Live>>> = Default::default();
    // True between flush(drain) and the drained callback. PipeWire's
    // set_active cancels a pending drain, so it must be re-armed after one.
    let draining = std::rc::Rc::new(std::cell::Cell::new(false));

    let build = {
        let (core, cons, stats, target, mainloop, ev, draining) =
            (core.clone(), cons.clone(), stats.clone(), target.clone(), mainloop.clone(), ev.clone(), draining.clone());
        move |rate: u32| -> anyhow::Result<Live> {
            let mut props = properties! {
                *pw::keys::MEDIA_TYPE => "Audio",
                *pw::keys::MEDIA_ROLE => "Music",
                *pw::keys::MEDIA_CATEGORY => "Playback",
                *pw::keys::APP_NAME => "Crate",
                *pw::keys::NODE_NAME => "crate",
                *pw::keys::AUDIO_CHANNELS => "2",
                "node.rate" => format!("1/{rate}"),
                "node.latency" => format!("{}/{rate}", rate / 50),
                // Crate owns its volume (saved in its config); don't let
                // WirePlumber's restore-stream put back an old level.
                "state.restore-props" => "false",
            };
            if let Some(t) = &target {
                props.insert("target.object", t.as_str());
                // Tests: never fall back to a real device if the target is missing.
                if std::env::var_os("CRATE_TARGET_STRICT").is_some() {
                    props.insert("node.dont-fallback", "true");
                    props.insert("node.dont-reconnect", "true");
                }
            }
            let stream = pw::stream::StreamBox::new(&core, "crate-playback", props)?;
            // SAFETY (spike): the stream borrows `core`, which lives for the
            // whole thread; we never drop core before the stream.
            let stream: pw::stream::StreamBox<'static> = unsafe { std::mem::transmute(stream) };
            let ml = mainloop.clone();
            let ev2 = ev.clone();
            let draining = draining.clone();
            let listener = stream
                .add_local_listener_with_user_data(Rt { cons: cons.clone(), stats: stats.clone(), eq: eq::Eq::default() })
                // A volume set before the format is negotiated is dropped, so
                // (re)apply it once the stream reaches Paused or Streaming.
                .state_changed(|stream, rt, _old, new| {
                    if matches!(new, pw::stream::StreamState::Paused | pw::stream::StreamState::Streaming) {
                        let v = rt.stats.volume();
                        let _ = stream.set_control(spa_sys::SPA_PROP_channelVolumes, &[v; CHANNELS]);
                    }
                })
                .process(|stream, rt| {
                    let t0 = Instant::now();
                    if let Ok(t) = stream.time() {
                        let r = t.rate();
                        if r.num == 1 && r.denom > 0 {
                            rt.stats.graph_rate.store(r.denom, Ordering::Relaxed);
                        }
                    }
                    let Some(mut buffer) = stream.dequeue_buffer() else { return };
                    let stride = 4 * CHANNELS;
                    let data = &mut buffer.datas_mut()[0];
                    let mut written = 0usize;
                    if let Some(slice) = data.data() {
                        let cap = slice.len() / stride;
                        let mut frames = 0;
                        // Uncontended except during a rebuild, when this stream is gone.
                        // EQ settings: recompute filters only when they or the rate change.
                        // Seqlock read, never spinning: if a write is in progress (odd) or
                        // landed meanwhile (changed), keep the old filters this cycle.
                        let gen_id = rt.stats.eq_gen.load(Ordering::Acquire);
                        let sr = rt.stats.stream_rate.load(Ordering::Relaxed);
                        if gen_id % 2 == 0 {
                            let gains = std::array::from_fn(|i| f32::from_bits(rt.stats.eq_gains[i].load(Ordering::Relaxed)));
                            let pre = f32::from_bits(rt.stats.eq_preamp.load(Ordering::Relaxed));
                            std::sync::atomic::fence(Ordering::Acquire);
                            if rt.stats.eq_gen.load(Ordering::Relaxed) == gen_id {
                                rt.eq.update(gen_id, sr, &gains, pre);
                            }
                        }
                        let eq_on = rt.eq.active();
                        if let Ok(mut cons) = rt.cons.try_lock() {
                            frames = cap.min(cons.slots() / CHANNELS);
                            if frames > 0 {
                                let chunk = cons.read_chunk(frames * CHANNELS).unwrap();
                                let (a, b) = chunk.as_slices();
                                for (i, s) in a.iter().chain(b).enumerate() {
                                    // Bypassed EQ writes the samples untouched (bit-perfect).
                                    let v = if eq_on { rt.eq.process(i % CHANNELS, *s) } else { *s };
                                    slice[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
                                }
                                chunk.commit_all();
                                rt.stats.got_data.store(true, Ordering::Release);
                            }
                        }
                        if frames < cap {
                            slice[frames * stride..cap * stride].fill(0);
                            if !rt.stats.decoder_done.load(Ordering::Acquire)
                                && !rt.stats.rebuilding.load(Ordering::Acquire)
                                && rt.stats.got_data.load(Ordering::Relaxed)
                            {
                                rt.stats.underrun_callbacks.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        rt.stats.frames_played.fetch_add(frames as u64, Ordering::Relaxed);
                        let sr = rt.stats.stream_rate.load(Ordering::Relaxed).max(1) as u64;
                        rt.stats.played_us.fetch_add(frames as u64 * 1_000_000 / sr, Ordering::Relaxed);
                        written = cap;
                    }
                    let chunk = data.chunk_mut();
                    *chunk.offset_mut() = 0;
                    *chunk.stride_mut() = stride as _;
                    *chunk.size_mut() = (stride * written) as _;
                    rt.stats.callbacks.fetch_add(1, Ordering::Relaxed);
                    rt.stats.max_cb_ns.fetch_max(t0.elapsed().as_nanos() as u64, Ordering::Relaxed);
                })
                .drained(move |_, rt| {
                    draining.set(false);
                    if rt.stats.final_drain.load(Ordering::Acquire) {
                        let _ = ev2.send(Event::Finished);
                        ml.quit();
                    } else {
                        rt.stats.drained.store(true, Ordering::Release);
                    }
                })
                .register()?;
            let values = format_param(rate);
            let mut params = [Pod::from_bytes(&values).unwrap()];
            stream.connect(
                spa::utils::Direction::Output,
                None,
                pw::stream::StreamFlags::AUTOCONNECT
                    | pw::stream::StreamFlags::MAP_BUFFERS
                    | pw::stream::StreamFlags::RT_PROCESS
                    | pw::stream::StreamFlags::INACTIVE,
                &mut params,
            )?;
            stats.stream_rate.store(rate, Ordering::Release);
            Ok((stream, listener))
        }
    };

    let live2 = live.clone();
    let ev2 = ev.clone();
    let ml_stop = mainloop.clone();
    let stats2 = stats.clone();
    let draining2 = draining.clone();
    // The stream runs only when it is buffered (ready) AND the caller hasn't paused.
    let ready = std::rc::Rc::new(std::cell::Cell::new(false));
    let apply = {
        let (live, ready, stats, draining) = (live.clone(), ready.clone(), stats.clone(), draining.clone());
        move || {
            if let Some((s, _)) = live.borrow().as_ref() {
                let _ = s.set_active(ready.get() && !stats.paused.load(Ordering::Acquire));
                if draining.get() {
                    // set_active cancelled the drain; re-arm it. If now paused,
                    // it completes once playback resumes.
                    let _ = s.flush(true);
                }
            }
        }
    };
    let _rx = cmd_rx.attach(mainloop.loop_(), move |cmd| match cmd {
        Cmd::Configure { rate, ack } => {
            // The old stream was drained by the decoder before this arrives.
            live2.borrow_mut().take();
            ready.set(false);
            draining2.set(false);
            match build(rate) {
                Ok(l) => {
                    let v = stats2.volume();
                    let _ = l.0.set_control(spa_sys::SPA_PROP_channelVolumes, &[v; CHANNELS]);
                    *live2.borrow_mut() = Some(l);
                }
                Err(e) => {
                    let _ = ev2.send(Event::Error(format!("stream: {e:#}")));
                }
            }
            let _ = ack.send(());
        }
        Cmd::Activate => {
            ready.set(true);
            apply();
        }
        Cmd::SetActive(on) => {
            stats2.paused.store(!on, Ordering::Release);
            apply();
        }
        Cmd::Volume => {
            if let Some((s, _)) = live2.borrow().as_ref() {
                let v = stats2.volume();
                let _ = s.set_control(spa_sys::SPA_PROP_channelVolumes, &[v; CHANNELS]);
            }
        }
        Cmd::Stop => {
            live2.borrow_mut().take();
            ml_stop.quit();
        }
        Cmd::Finish => {
            let _ = ev2.send(Event::Finished);
            ml_stop.quit();
        }
        Cmd::Drain { last } => {
            stats2.final_drain.store(last, Ordering::Release);
            let flushed = live2.borrow().as_ref().map(|(s, _)| s.flush(true).is_ok()).unwrap_or(false);
            draining2.set(flushed);
            if !flushed {
                // No stream to drain: complete immediately.
                if last {
                    let _ = ev2.send(Event::Finished);
                    ml_stop.quit();
                } else {
                    stats2.drained.store(true, Ordering::Release);
                }
            }
        }
    });

    mainloop.run();
    live.borrow_mut().take();
    Ok(())
}

#[cfg(test)]
mod fade_tests {
    use super::{CHANNELS, mix_fade};
    use std::collections::VecDeque;

    #[test]
    fn equal_power_fade() {
        let total = 1000;
        // Fading a full-scale tail into silence follows cos: ~1 at the start, ~0 at the end.
        let mut from: VecDeque<f32> = std::iter::repeat_n(1.0, total * CHANNELS).collect();
        let mut into = vec![0.0f32; total * CHANNELS];
        assert_eq!(mix_fade(&mut into, &mut from, 0, total), total);
        assert!(from.is_empty());
        // The limiter is off at the edges: full scale passes at the start.
        assert!(into[0] > 0.999 && into[into.len() - 1] < 0.002);
        assert!(into.windows(2).all(|w| w[1] <= w[0] + 1e-6));
        // Silence into a full-scale track follows sin; split across two calls it lines up.
        let mut from: VecDeque<f32> = std::iter::repeat_n(0.0, total * CHANNELS).collect();
        let mut a = vec![1.0f32; 400 * CHANNELS];
        let mut b = vec![1.0f32; 600 * CHANNELS];
        let n = mix_fade(&mut a, &mut from, 0, total);
        mix_fade(&mut b, &mut from, n, total);
        let joined: Vec<f32> = a.into_iter().chain(b).collect();
        assert!(joined[0] < 0.002 && joined[joined.len() - 1] > 0.999);
        assert!(joined.windows(2).all(|w| w[1] + 1e-6 >= w[0]));
        // Runs out of tail: stops mixing and leaves the rest of `into` as is.
        let mut from: VecDeque<f32> = std::iter::repeat_n(0.5, 10 * CHANNELS).collect();
        let mut into = vec![0.25f32; 20 * CHANNELS];
        assert_eq!(mix_fade(&mut into, &mut from, 990, 1000), 10);
        assert!(into[10 * CHANNELS..].iter().all(|&x| x == 0.25));
        // Two full-scale signals mid-fade stay under 1.0 without flat clipping.
        let mut from: VecDeque<f32> = std::iter::repeat_n(1.0, 2 * CHANNELS).collect();
        let mut into = vec![1.0f32; 2 * CHANNELS];
        mix_fade(&mut into, &mut from, 499, 1000);
        assert!(into.iter().all(|&x| x < 1.0 && x > 0.95));
        // In-phase full scale at every point of the fade: under 1.0 before any
        // clamp, rising with the sum, identity at the edges.
        use super::fade_limit;
        let half_pi = std::f32::consts::FRAC_PI_2;
        let mut prev = 0.0;
        for i in 1..100 {
            let t = i as f32 / 100.0;
            let m = (t * half_pi).cos() + (t * half_pi).sin();
            let y = fade_limit(m, m);
            assert!(y < 1.0, "t={t}: {y}");
            assert!(y >= prev - 0.02);
            prev = y;
            // Anything under 2 - m is untouched.
            assert_eq!(fade_limit((2.0 - m) * 0.99, m), (2.0 - m) * 0.99);
        }
        assert_eq!(fade_limit(1.0, 1.0), 1.0);
        assert_eq!(fade_limit(-0.8, 1.0), -0.8);
    }
}
