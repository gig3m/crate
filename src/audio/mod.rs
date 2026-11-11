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
pub mod mp4gap;
pub mod opus;
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
    /// `frames * 1e6 % rate` not yet counted in `queued_us` (no drift).
    queued_rem: &'a mut u64,
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
        let r = rate.max(1) as u64;
        let num = frames * 1_000_000 + *self.queued_rem;
        *self.queued_us += num / r;
        *self.queued_rem = num % r;
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
    /// Stop flags of the prefetches this engine started (stop() sets them).
    pub prefetches: Mutex<Vec<Arc<AtomicBool>>>,
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
    /// The last few thousand frames sent to PipeWire, for visualizers.
    pub scope: Scope,
}

/// A lock-free tap of the output: the RT thread overwrites a ring of
/// interleaved stereo samples (f32 bits) and bumps `head`; readers copy the
/// latest frames. A torn read only costs a visual glitch.
pub struct Scope {
    ring: Box<[AtomicU32]>,
    head: AtomicU64,
    /// Rate of the samples in the ring (Hz).
    pub rate: AtomicU32,
}

/// Frames the scope keeps.
pub const SCOPE_FRAMES: usize = 4096;

impl Default for Scope {
    fn default() -> Self {
        Self { ring: (0..SCOPE_FRAMES * CHANNELS).map(|_| AtomicU32::new(0)).collect(), head: AtomicU64::new(0), rate: AtomicU32::new(0) }
    }
}

impl std::fmt::Debug for Scope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Scope")
    }
}

impl Scope {
    #[inline]
    fn push(&self, at: u64, sample: f32) {
        let n = self.ring.len() as u64;
        self.ring[(at % n) as usize].store(sample.to_bits(), Ordering::Relaxed);
    }

    /// Samples written so far: unchanged means nothing new played.
    pub fn head(&self) -> u64 {
        self.head.load(Ordering::Acquire)
    }

    /// The latest `frames` frames (oldest first), interleaved stereo, into `out`.
    /// Returns false when nothing has played yet.
    pub fn latest(&self, frames: usize, out: &mut Vec<f32>) -> bool {
        let frames = frames.min(SCOPE_FRAMES);
        let head = self.head.load(Ordering::Acquire);
        out.clear();
        if head == 0 {
            return false;
        }
        let n = self.ring.len() as u64;
        let want = (frames * CHANNELS) as u64;
        let start = head.saturating_sub(want);
        out.extend((start..head).map(|i| f32::from_bits(self.ring[(i % n) as usize].load(Ordering::Relaxed))));
        true
    }
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
    /// A queued track (engine index) couldn't be opened and was skipped.
    /// A track was skipped: it couldn't be opened, or (`partial`) it failed
    /// part-way and the queue moved on.
    Skipped { index: usize, error: String, partial: bool },
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
        // Prefetches end now, not when the decoder thread gets round to
        // exiting (it may be waiting on the network).
        for f in self.stats.prefetches.lock().unwrap().iter() {
            f.store(true, Ordering::Release);
        }
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
    let mut queued_rem = 0u64;
    let mut scratch: Vec<f32> = Vec::new();
    // Prefetches this engine started: stopped when it exits, however it exits.
    let mut prefetches = source::Prefetches::default();
    // A non-stereo track's interleaved frames before the mix to stereo.
    let mut wide: Vec<f32> = Vec::new();
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

    'tracks: loop {
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
                    let mut out = Out { prod: &mut prod, cap, activated: &mut activated, queued_frames: &mut queued_frames, queued_us: &mut queued_us, queued_rem: &mut queued_rem, stream_frames: &mut stream_frames };
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
        // A track that fails after it was opened (a decoder it can't make, a
        // network drop mid-stream, too many corrupt frames): skip it and carry
        // on with the queue, unless it's the first track and nothing has played
        // (that's the error to report).
        macro_rules! track_failed {
            ($label:lifetime, $played:expr, $err:expr) => {{
                let e: anyhow::Error = $err;
                // A stop (seek, skip, new queue) while a read was waiting
                // surfaces as an error too: just stop, don't open the next.
                if stats.stop.load(Ordering::Acquire) {
                    return Ok(());
                }
                if index == 0 && $played == 0 {
                    return Err(e);
                }
                eprintln!("[engine] {} failed part-way: {e:#}; skipping to the next track", src.label());
                let _ = ev.send(Event::Skipped { index, error: format!("{e:#}"), partial: $played > 0 });
                // What's held back plays as is, then the rest of the previous
                // (healthy) track's end: unchanged if the fade into this one
                // hadn't begun, else fading out on its own. Crossfades only
                // happen at one rate, so both are at `tail_rate`.
                for part in 0..2 {
                    drained.clear();
                    if part == 0 {
                        drained.extend(tail.drain(..));
                    } else if fade_done == 0 {
                        drained.extend(fading.drain(..));
                    } else if !fading.is_empty() {
                        drained.resize(fading.len() / CHANNELS * CHANNELS, 0.0);
                        mix_fade(&mut drained, &mut fading, fade_done, fade_total);
                        fading.clear();
                    }
                    if drained.is_empty() {
                        continue;
                    }
                    let mut out = Out { prod: &mut prod, cap, activated: &mut activated, queued_frames: &mut queued_frames, queued_us: &mut queued_us, queued_rem: &mut queued_rem, stream_frames: &mut stream_frames };
                    if !out.push(&drained, tail_rate, &cmd_tx, &stats) {
                        return Ok(());
                    }
                }
                continue $label;
            }};
        }
        // A gain that isn't a sane finite factor is ignored rather than trusted.
        let gain = if gain.is_finite() && gain > 0.0 && gain <= 4.0 { gain } else { 1.0 };
        let open = || -> anyhow::Result<_> {
            let raw = src.open(stats.stop.clone())?;
            // MP4: watch for the edit list (gapless AAC; see mp4gap).
            let is_mp4 = src.ext().is_some_and(|e| matches!(e.as_str(), "m4a" | "mp4" | "m4b"));
            let (media, edits): (Box<dyn symphonia::core::io::MediaSource>, _) = if is_mp4 {
                let (s, found) = mp4gap::Sniff::new(raw);
                (Box::new(s), Some(found))
            } else {
                (raw, None)
            };
            let mss = MediaSourceStream::new(media, Default::default());
            let mut hint = Hint::new();
            if let Some(ext) = src.ext() {
                hint.with_extension(&ext);
            }
            let fmt = symphonia::default::get_probe().probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())?;
            let track = fmt.default_track(TrackType::Audio).ok_or_else(|| anyhow::anyhow!("no audio track"))?.clone();
            Ok((fmt, track, edits))
        };
        // The first track failing is the error to report. A later one
        // failing (a server hiccup, a 503) mustn't cut off what's still
        // buffered: retry once, then skip it and carry on with the queue.
        let (mut fmt, track, edits) = match open() {
            Ok(x) => x,
            Err(e) if index > 0 && !stats.stop.load(Ordering::Acquire) => {
                std::thread::sleep(Duration::from_millis(500));
                match open() {
                    Ok(x) => x,
                    Err(_) if stats.stop.load(Ordering::Acquire) => return Ok(()),
                    Err(e2) => {
                        eprintln!("[engine] skipping {}: {e:#}; retry: {e2:#}", src.label());
                        let _ = ev.send(Event::Skipped { index, error: format!("{e2:#}"), partial: false });
                        // The held end was kept for a fade into the skipped
                        // track; the next one's fade was decided against it
                        // too. Play the end as is, so nothing crossfades.
                        if !tail.is_empty() {
                            drained.clear();
                            drained.extend(tail.drain(..));
                            let mut out = Out { prod: &mut prod, cap, activated: &mut activated, queued_frames: &mut queued_frames, queued_us: &mut queued_us, queued_rem: &mut queued_rem, stream_frames: &mut stream_frames };
                            if !out.push(&drained, tail_rate, &cmd_tx, &stats) {
                                return Ok(());
                            }
                        }
                        continue;
                    }
                }
            }
            Err(e) => return Err(e),
        };
        let Some(CodecParameters::Audio(params)) = &track.codec_params else {
            track_failed!('tracks, 0, anyhow::anyhow!("not audio"))
        };
        let rate = params.sample_rate.unwrap_or(48_000);
        let bits = params.bits_per_sample.unwrap_or(0);

        // Crossfade into this track only at the same rate, with a held-back
        // end to fade from, and not when starting mid-track (a seek).
        let fade_frames = (item.fade_ms as u64 * rate as u64 / 1000) as usize;
        // The track must be known to outlast the fade twice over, or the fade
        // would run past its end (into the next track); unknown length: gapless.
        // The MP4 edit list's priming and real length (exact, when present).
        let edit_trims = edits.and_then(|f| f.lock().unwrap().trims(rate)).filter(|(d, t)| *d > 0 || *t > 0);
        let num_frames = edit_trims.map(|(_, t)| t).or_else(|| src.len_ms().map(|ms| ms * rate as u64 / 1000)).or(track.num_frames);
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
                let mut out = Out { prod: &mut prod, cap, activated: &mut activated, queued_frames: &mut queued_frames, queued_us: &mut queued_us, queued_rem: &mut queued_rem, stream_frames: &mut stream_frames };
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
            let mut out = Out { prod: &mut prod, cap, activated: &mut activated, queued_frames: &mut queued_frames, queued_us: &mut queued_us, queued_rem: &mut queued_rem, stream_frames: &mut stream_frames };
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
        // Opus in Ogg: the reader lands exactly on the target with no
        // pre-roll, so seek 80 ms early and drop what that decodes (RFC 7845
        // §4.6), letting the decoder settle before audio is heard.
        let ogg_opus = params.codec == symphonia::core::codecs::audio::well_known::CODEC_ID_OPUS && src.ext().as_deref() == Some("ogg");
        // A transcode Plex started at the seek point (whole seconds): no
        // seek; the part of a second before the target is dropped instead.
        let src_start = if index == 0 { src.start_ms().min(start_ms) } else { 0 };
        let preroll_ms = if src_start > 0 {
            start_ms - src_start
        } else if ogg_opus && index == 0 {
            start_ms.min(80)
        } else {
            0
        };
        let mut preroll_frames = (preroll_ms * rate as u64 / 1000) as usize;
        // Seeking to (or pre-rolling back to) 0 is just playing from the start.
        let seeked = index == 0 && src_start == 0 && start_ms > preroll_ms;
        if seeked {
            use symphonia::core::formats::{SeekMode, SeekTo};
            use symphonia::core::units::Time;
            // MP4 with an edit list: the raw timeline starts with the priming.
            let priming_s = edit_trims.map(|(d, _)| d as f64 / rate as f64).unwrap_or(0.0);
            let time = Time::try_from_secs_f64((start_ms - preroll_ms) as f64 / 1000.0 + priming_s).unwrap_or_default();
            if let Err(e) = fmt.seek(SeekMode::Accurate, SeekTo::Time { time, track_id: Some(track.id) }) {
                track_failed!('tracks, 0, e.into())
            }
        }

        // Hold back this track's end only if the next queued track will fade
        // in (checked now; if the queue changes, the end just plays as is).
        let hold_for = |q: &Queue| q.items.front().map(|n| (n.fade_ms.min(12_000) as u64 * rate as u64 / 1000) as usize * CHANNELS).unwrap_or(0);
        let mut hold = hold_for(&queue.lock().unwrap());
        // Re-checked every packet (an uncontended lock): editing the queue can
        // change whether the next track fades in, right up to the end.
        tail_rate = rate;
        let mut dec = match opus::codecs().make_audio_decoder(params, &AudioDecoderOptions::default().gapless(true)) {
            Ok(d) => d,
            Err(e) => track_failed!('tracks, 0, anyhow::Error::from(e).context("no decoder for this format")),
        };
        if seeked && ogg_opus {
            // The decoder starts mid-stream: no start-of-stream trims (Opus
            // pre-skip) on the first packets it sees.
            dec.reset();
        }
        let limit_frames = track_limit.map(|s| (s * rate as f64) as u64);
        let mut track_frames = 0u64;
        let mut bad_packets = 0u32;
        // Fetch the next track once this one is 10 s in (halfway, if
        // shorter): late enough that a seek or skip right after a start
        // doesn't throw it away, and this track's own download has a head
        // start on the connection.
        // Counted in played time (the decoder runs several seconds ahead).
        let prefetch_after_us = num_frames.map(|n| (n / 2).min(10 * rate as u64)).unwrap_or(10 * rate as u64) * 1_000_000 / rate as u64;
        let track_start_us = queued_us;
        let mut prefetched = false;
        // Symphonia's Ogg reader returns UnexpectedEof, not Ok(None), when the
        // last page isn't flagged end-of-stream, and Plex never flags it.
        // Where decoded output starts on the track (the pre-roll is dropped).
        let start_frames = if index == 0 { start_ms * rate as u64 / 1000 } else { 0 };
        let mut edit = edit_trims.map(|(delay, total)| mp4gap::EditList::new(delay, total, seeked, start_frames));
        loop {
            let packet = match fmt.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => break,
                Err(symphonia::core::errors::Error::IoError(e)) if ogg_opus && e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    // Plex's Ogg has no end-of-stream flag, so a cut-off stream
                    // ends the same way: compare with the known length (2 s +
                    // 1% slack: Plex's durations can run a little long). Short:
                    // say so and drop a cached copy that would end early every
                    // time, but carry on with the queue like any track end.
                    let at = start_frames + track_frames;
                    if let Some(n) = num_frames
                        && at + 2 * rate as u64 + n / 100 < n
                    {
                        eprintln!("[engine] {} ended early at {:.1} of {:.1} s", src.label(), at as f64 / rate as f64, n as f64 / rate as f64);
                        if let Some(p) = src.cache_path() {
                            let _ = std::fs::remove_file(p);
                        }
                    }
                    break;
                }
                Err(e) => track_failed!('tracks, track_frames, e.into()),
            };
            if stats.stop.load(Ordering::Acquire) {
                return Ok(());
            }
            if limit_frames.is_some_and(|l| track_frames >= l) {
                break;
            }
            if packet.track_id != track.id {
                continue;
            }
            let buf = match dec.decode(&packet) {
                Ok(b) => {
                    bad_packets = 0;
                    b
                }
                // A corrupt frame: drop it and go on (symphonia's decode
                // errors are recoverable); a run of them is a broken file.
                Err(symphonia::core::errors::Error::DecodeError(e)) => {
                    bad_packets += 1;
                    if bad_packets > 20 {
                        track_failed!('tracks, track_frames, anyhow::anyhow!("too many corrupt frames ({e})"))
                    }
                    eprintln!("[engine] {}: dropped a corrupt frame ({e})", src.label());
                    continue;
                }
                Err(e) => track_failed!('tracks, track_frames, e.into()),
            };
            let ch = buf.spec().channels().count();
            if ch == CHANNELS {
                scratch.resize(buf.frames() * ch, 0.0);
                buf.copy_to_slice_interleaved(&mut scratch[..]);
            } else {
                // Mono is copied to both sides (still bit-exact); more than
                // two channels are mixed down to stereo.
                let weights = stereo_weights(buf.spec().channels());
                wide.resize(buf.frames() * ch, 0.0);
                buf.copy_to_slice_interleaved(&mut wide[..]);
                to_stereo(&wide, &weights, &mut scratch);
            }
            if let Some(e) = edit.as_mut() {
                e.apply(&mut scratch, CHANNELS);
                if scratch.is_empty() {
                    if e.left == Some(0) {
                        break; // the real audio has ended; the rest is padding
                    }
                    continue;
                }
            }
            if preroll_frames > 0 {
                let drop = preroll_frames.min(scratch.len() / CHANNELS);
                scratch.drain(..drop * CHANNELS);
                preroll_frames -= drop;
                if scratch.is_empty() {
                    continue;
                }
            }
            // Loudness leveling. Exactly 1.0 skips the loop, so leveling off
            // stays bit-perfect.
            if gain != 1.0 {
                for s in scratch.iter_mut() {
                    *s = (*s * gain).clamp(-1.0, 1.0);
                }
            }
            track_frames += (scratch.len() / CHANNELS) as u64;
            if !prefetched && stats.played_us.load(Ordering::Acquire) >= track_start_us + prefetch_after_us {
                prefetched = true;
                let next = queue.lock().unwrap().items.front().map(|n| n.src.clone());
                if let Some(next) = next {
                    let flag = next.prefetch(|flag| {
                        // Registered before the worker starts; a stop that
                        // landed just before the push missed it.
                        stats.prefetches.lock().unwrap().push(flag.clone());
                        if stats.stop.load(Ordering::Acquire) {
                            flag.store(true, Ordering::Release);
                        }
                    });
                    prefetches.add(flag);
                }
            }
            // The previous track's end fades out under this one's start.
            if !fading.is_empty() {
                fade_done += mix_fade(&mut scratch, &mut fading, fade_done, fade_total);
            }
            hold = hold_for(&queue.lock().unwrap());
            let mut out = Out { prod: &mut prod, cap, activated: &mut activated, queued_frames: &mut queued_frames, queued_us: &mut queued_us, queued_rem: &mut queued_rem, stream_frames: &mut stream_frames };
            // Anything already held goes out first, in order, even if the hold
            // has since dropped to nothing.
            if hold == 0 && tail.is_empty() {
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
        // A transcode started at a seek point is only good for another seek
        // to that exact second: don't keep it in the cache. And a stream that
        // ran well past the track's length (Plex ignoring the offset, say)
        // isn't trusted again either.
        if let Some(p) = src.cache_path() {
            let at = start_frames + track_frames;
            let too_long = ogg_opus && num_frames.is_some_and(|n| at > n + 2 * rate as u64 + n / 100);
            if too_long {
                eprintln!("[engine] {} ran past its length ({:.1} s): not keeping it", src.label(), at as f64 / rate as f64);
            }
            if too_long || src.start_ms() > 0 {
                let _ = std::fs::remove_file(p);
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
                let mut out = Out { prod: &mut prod, cap, activated: &mut activated, queued_frames: &mut queued_frames, queued_us: &mut queued_us, queued_rem: &mut queued_rem, stream_frames: &mut stream_frames };
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
        let mut out = Out { prod: &mut prod, cap, activated: &mut activated, queued_frames: &mut queued_frames, queued_us: &mut queued_us, queued_rem: &mut queued_rem, stream_frames: &mut stream_frames };
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
    /// `frames * 1e6 % rate` not yet counted in `played_us`: small
    /// callbacks would otherwise lose up to a microsecond each.
    played_rem: u64,
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
                .add_local_listener_with_user_data(Rt { cons: cons.clone(), stats: stats.clone(), eq: eq::Eq::default(), played_rem: 0 })
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
                    // Frames the graph wants this cycle (about one quantum).
                    // Filling the whole buffer instead (sized for the largest
                    // quantum: 12288 frames measured) queues ~a quarter second
                    // per callback, which the output tap and the played-time
                    // clock then run ahead of. 0 = not told: fill it all.
                    let requested = buffer.requested() as usize;
                    let stride = 4 * CHANNELS;
                    let data = &mut buffer.datas_mut()[0];
                    let mut written = 0usize;
                    if let Some(slice) = data.data() {
                        let room = slice.len() / stride;
                        let cap = if requested > 0 { requested.min(room) } else { room };
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
                                let scope = &rt.stats.scope;
                                let base = scope.head.load(Ordering::Relaxed);
                                for (i, s) in a.iter().chain(b).enumerate() {
                                    // Bypassed EQ writes the samples untouched (bit-perfect).
                                    let v = if eq_on { rt.eq.process(i % CHANNELS, *s) } else { *s };
                                    slice[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
                                    scope.push(base + i as u64, v);
                                }
                                scope.rate.store(sr, Ordering::Relaxed);
                                scope.head.store(base + (frames * CHANNELS) as u64, Ordering::Release);
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
                        let num = frames as u64 * 1_000_000 + rt.played_rem;
                        rt.stats.played_us.fetch_add(num / sr, Ordering::Relaxed);
                        rt.played_rem = num % sr;
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

/// Each channel's (left, right) weight when mixing to stereo: mono goes to
/// both sides at full level; surround uses the usual -3 dB for centre and
/// rear/side channels, drops LFE, and is scaled so a full-scale signal on
/// every channel can't clip.
fn stereo_weights(ch: &symphonia::core::audio::Channels) -> Vec<(f32, f32)> {
    use symphonia::core::audio::Channels;
    const H: f32 = std::f32::consts::FRAC_1_SQRT_2;
    let n = ch.count();
    if n == 1 {
        return vec![(1.0, 1.0)];
    }
    let mut w: Vec<(f32, f32)> = match ch {
        Channels::Positioned(p) => (0..32u32)
            .filter(|b| p.bits() & (1 << b) != 0)
            .map(|b| match b {
                0 => (1.0, 0.0),                        // front left
                1 => (0.0, 1.0),                        // front right
                3 | 18 => (0.0, 0.0),                   // LFE
                4 | 6 | 9 | 12 | 15 | 19 | 22 | 24 => (H, 0.0), // left-side positions
                5 | 7 | 10 | 14 | 17 | 20 | 23 | 25 => (0.0, H), // right-side positions
                _ => (H, H),                            // centres
            })
            .collect(),
        // Unlabelled channels: the first two are left and right.
        _ => (0..n).map(|i| match i { 0 => (1.0, 0.0), 1 => (0.0, 1.0), _ => (0.0, 0.0) }).collect(),
    };
    w.resize(n, (0.0, 0.0));
    let (l, r) = w.iter().fold((0.0f32, 0.0f32), |(l, r), (a, b)| (l + a, r + b));
    let peak = l.max(r);
    if peak > 1.0 {
        for x in &mut w {
            x.0 /= peak;
            x.1 /= peak;
        }
    }
    w
}

/// Mix interleaved frames of `weights.len()` channels into stereo.
fn to_stereo(wide: &[f32], weights: &[(f32, f32)], out: &mut Vec<f32>) {
    let n = weights.len().max(1);
    out.clear();
    out.reserve(wide.len() / n * 2);
    for frame in wide.chunks_exact(n) {
        let (mut l, mut r) = (0.0f32, 0.0f32);
        for (s, (a, b)) in frame.iter().zip(weights) {
            l += s * a;
            r += s * b;
        }
        out.push(l);
        out.push(r);
    }
}

#[cfg(test)]
mod stereo_tests {
    use symphonia::core::audio::{Channels, Position};

    #[test]
    fn mono_is_copied_exactly() {
        let w = super::stereo_weights(&Channels::Positioned(Position::FRONT_CENTER));
        let mut out = Vec::new();
        super::to_stereo(&[0.25, -0.5], &w, &mut out);
        assert_eq!(out, vec![0.25, 0.25, -0.5, -0.5]);
    }

    #[test]
    fn surround_downmix_cannot_clip() {
        let p = Position::FRONT_LEFT | Position::FRONT_RIGHT | Position::FRONT_CENTER | Position::LFE1 | Position::REAR_LEFT | Position::REAR_RIGHT;
        let w = super::stereo_weights(&Channels::Positioned(p));
        assert_eq!(w.len(), 6);
        assert_eq!(w[3], (0.0, 0.0), "LFE dropped");
        let mut out = Vec::new();
        super::to_stereo(&[1.0; 6], &w, &mut out);
        assert!(out.iter().all(|x| *x <= 1.0 + 1e-6), "{out:?}");
        // Left-only content stays on the left.
        super::to_stereo(&[1.0, 0.0, 0.0, 0.0, 0.0, 0.0], &w, &mut out);
        assert!(out[0] > 0.3 && out[1] == 0.0);
    }
}
