//! Crate audio engine (spike quality, shaped for the real app).
//!
//! Threads:
//! - decoder: pops the shared queue, decodes with symphonia, pushes f32 frames into
//!   an rtrb ring. Asks the PipeWire thread to rebuild the stream when the
//!   sample rate changes between tracks.
//! - pipewire: owns the main loop and the stream. The process callback (RT)
//!   only copies from the ring, counts frames and samples the graph rate.
//! - caller: reads `Stats` and receives `Event`s.

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
    TrackStarted { index: usize, label: String, source_rate: u32, bits: u32, at_frame: u64, at_us: u64 },
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
    items: VecDeque<Source>,
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

    /// Linear volume 0.0..=1.0 (PipeWire software volume; 1.0 leaves the
    /// samples untouched, anything lower is no longer bit-perfect).
    pub fn set_volume(&self, linear: f32) {
        let att = ((1.0 - linear.clamp(0.0, 1.0)) * 1_000_000.0).round() as u32;
        self.stats.attenuation.store(att, Ordering::Release);
        let _ = self.cmd.send(Cmd::Volume);
    }

    /// Add tracks to the end. False once the decoder has finished the queue
    /// (the caller must start a new engine for them).
    pub fn append(&self, more: Vec<Source>) -> bool {
        let mut q = self.queue.lock().unwrap();
        if q.closed {
            return false;
        }
        q.items.extend(more);
        true
    }

    /// How many tracks the decoder has opened so far.
    pub fn started(&self) -> usize {
        self.queue.lock().unwrap().started
    }

    /// Replace the tracks not opened yet, but only if the decoder has opened
    /// exactly `started` tracks (so the caller's view of what's next is
    /// current). False means retry with a fresh `started()`, or the queue closed.
    pub fn replace_upcoming(&self, started: usize, upcoming: Vec<Source>) -> bool {
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
    pub fn play(queue: Vec<Source>, target: Option<String>) -> anyhow::Result<Self> {
        Self::play_from(queue, 0, target)
    }

    /// Like `play`, starting `start_ms` into the first track.
    pub fn play_from(queue: Vec<Source>, start_ms: u64, target: Option<String>) -> anyhow::Result<Self> {
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
        let (index, src) = match popped {
            Some(next) => next,
            None => {
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
        });
        if index == 0 && start_ms > 0 {
            use symphonia::core::formats::{SeekMode, SeekTo};
            use symphonia::core::units::Time;
            let time = Time::try_from_secs_f64(start_ms as f64 / 1000.0).unwrap_or_default();
            fmt.seek(SeekMode::Accurate, SeekTo::Time { time, track_id: Some(track.id) })?;
        }

        let prefill = (rate as usize / 10) * CHANNELS; // 100 ms
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
            let mut off = 0;
            while off < scratch.len() {
                let free = prod.slots();
                if free == 0 {
                    // A full ring on a stream that hasn't started can't drain: start it.
                    if !activated {
                        let _ = cmd_tx.send(Cmd::Activate);
                        activated = true;
                    }
                    if stats.stop.load(Ordering::Acquire) {
                        return Ok(());
                    }
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                let take = free.min(scratch.len() - off);
                prod.write_chunk_uninit(take)
                    .unwrap()
                    .fill_from_iter(scratch[off..off + take].iter().copied());
                off += take;
            }
            if !activated && cap - prod.slots() >= prefill {
                activated = true;
                let _ = cmd_tx.send(Cmd::Activate);
            }
            queued_frames += (scratch.len() / CHANNELS) as u64;
            stream_frames += (scratch.len() / CHANNELS) as u64;
            queued_us += (scratch.len() / CHANNELS) as u64 * 1_000_000 / rate as u64;
            track_frames += (scratch.len() / CHANNELS) as u64;
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
            }
            let stream = pw::stream::StreamBox::new(&core, "crate-playback", props)?;
            // SAFETY (spike): the stream borrows `core`, which lives for the
            // whole thread; we never drop core before the stream.
            let stream: pw::stream::StreamBox<'static> = unsafe { std::mem::transmute(stream) };
            let ml = mainloop.clone();
            let ev2 = ev.clone();
            let draining = draining.clone();
            let listener = stream
                .add_local_listener_with_user_data(Rt { cons: cons.clone(), stats: stats.clone() })
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
                        if let Ok(mut cons) = rt.cons.try_lock() {
                            frames = cap.min(cons.slots() / CHANNELS);
                            if frames > 0 {
                                let chunk = cons.read_chunk(frames * CHANNELS).unwrap();
                                let (a, b) = chunk.as_slices();
                                for (i, s) in a.iter().chain(b).enumerate() {
                                    slice[i * 4..i * 4 + 4].copy_from_slice(&s.to_le_bytes());
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
