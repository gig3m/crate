//! Aulos audio engine (spike quality, shaped for the real app).
//!
//! Threads:
//! - decoder: walks the queue, decodes with symphonia, pushes f32 frames into
//!   an rtrb ring. Asks the PipeWire thread to rebuild the stream when the
//!   sample rate changes between tracks.
//! - pipewire: owns the main loop and the stream. The process callback (RT)
//!   only copies from the ring, counts frames and samples the graph rate.
//! - caller: reads `Stats` and receives `Event`s.

pub mod source;

use std::{
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
    pub stop: AtomicBool,
    /// Ring is being emptied on purpose ahead of a stream rebuild.
    pub rebuilding: AtomicBool,
}

#[derive(Debug)]
pub enum Event {
    TrackStarted { index: usize, label: String, source_rate: u32, bits: u32, at_frame: u64 },
    Reconnected { from: u32, to: u32, silent_ms: f64 },
    FirstAudio { ms_since_play: f64 },
    Finished,
    Error(String),
}

enum Cmd {
    /// Rebuild the stream at `rate`, created inactive.
    Configure { rate: u32, ack: mpsc::Sender<()> },
    /// Start pulling (sent once the ring holds enough audio).
    Activate,
    Drain,
    Stop,
}

pub struct Engine {
    pub stats: Arc<Stats>,
    pub events: mpsc::Receiver<Event>,
    cmd: pw::channel::Sender<Cmd>,
}

impl Engine {
    pub fn stop(&self) {
        self.stats.stop.store(true, Ordering::Release);
        let _ = self.cmd.send(Cmd::Stop);
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
        let stats = Arc::new(Stats::default());
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
            let (stats, ev_tx) = (stats.clone(), ev_tx.clone());
            std::thread::Builder::new()
                .name("decoder".into())
                .spawn(move || {
                    if let Err(e) = decoder_thread(queue, prod, cmd_tx, stats.clone(), ev_tx.clone()) {
                        let _ = ev_tx.send(Event::Error(format!("decoder: {e:#}")));
                    }
                    stats.decoder_done.store(true, Ordering::Release);
                })?;
        }
        Ok(Self { stats, events, cmd })
    }
}

fn decoder_thread(
    queue: Vec<Source>,
    mut prod: rtrb::Producer<f32>,
    cmd_tx: pw::channel::Sender<Cmd>,
    stats: Arc<Stats>,
    ev: mpsc::Sender<Event>,
) -> anyhow::Result<()> {
    let play_started = Instant::now();
    let mut cur_rate = 0u32;
    let mut queued_frames = 0u64;
    let mut scratch: Vec<f32> = Vec::new();
    let mut first_audio_reported = false;
    // Spike/testing aid: cap each track at N seconds.
    let track_limit: Option<f64> = std::env::var("AULOS_TRACK_LIMIT").ok().and_then(|v| v.parse().ok());

    for (index, src) in queue.into_iter().enumerate() {
        let mss = MediaSourceStream::new(src.open()?, Default::default());
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

        let rebuilt = rate != cur_rate;
        if rebuilt {
            // Let everything queued at the old rate play out, then rebuild.
            stats.rebuilding.store(true, Ordering::Release);
            let t_wait = Instant::now();
            while prod.slots() < prod.buffer().capacity() {
                if stats.stop.load(Ordering::Acquire) {
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            let t_empty = Instant::now();
            stats.got_data.store(false, Ordering::Release);
            let (ack_tx, ack_rx) = mpsc::channel();
            cmd_tx
                .send(Cmd::Configure { rate, ack: ack_tx })
                .map_err(|_| anyhow::anyhow!("pipewire thread gone"))?;
            ack_rx.recv()?;
            stats.rebuilding.store(false, Ordering::Release);
            if cur_rate != 0 {
                // Silence = ring empty -> first real audio at the new rate.
                let from = cur_rate;
                let ev2 = ev.clone();
                let st = stats.clone();
                std::thread::spawn(move || {
                    while !st.got_data.load(Ordering::Acquire) {
                        std::thread::sleep(Duration::from_micros(200));
                    }
                    let _ = ev2.send(Event::Reconnected {
                        from,
                        to: rate,
                        silent_ms: t_empty.elapsed().as_secs_f64() * 1000.0,
                    });
                });
                let _ = t_wait;
            }
            cur_rate = rate;
        }
        let _ = ev.send(Event::TrackStarted {
            index,
            label: src.label(),
            source_rate: rate,
            bits,
            at_frame: queued_frames,
        });

        let mut needs_activate = rebuilt;
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
            if needs_activate && prod.buffer().capacity() - prod.slots() >= prefill {
                needs_activate = false;
                let _ = cmd_tx.send(Cmd::Activate);
            }
            queued_frames += (scratch.len() / CHANNELS) as u64;
            track_frames += (scratch.len() / CHANNELS) as u64;
            if !first_audio_reported && stats.got_data.load(Ordering::Acquire) {
                first_audio_reported = true;
                let _ = ev.send(Event::FirstAudio {
                    ms_since_play: play_started.elapsed().as_secs_f64() * 1000.0,
                });
            }
        }
    }
    let _ = cmd_tx.send(Cmd::Activate);
    // Nothing more is coming: an empty ring from here on is the end, not an underrun.
    stats.decoder_done.store(true, Ordering::Release);
    // Wait for the ring to empty, then ask PipeWire to drain its buffers.
    while prod.slots() < prod.buffer().capacity() {
        if stats.stop.load(Ordering::Acquire) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let _ = cmd_tx.send(Cmd::Drain);
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

    let build = {
        let (core, cons, stats, target, mainloop, ev) =
            (core.clone(), cons.clone(), stats.clone(), target.clone(), mainloop.clone(), ev.clone());
        move |rate: u32| -> anyhow::Result<Live> {
            let mut props = properties! {
                *pw::keys::MEDIA_TYPE => "Audio",
                *pw::keys::MEDIA_ROLE => "Music",
                *pw::keys::MEDIA_CATEGORY => "Playback",
                *pw::keys::APP_NAME => "Aulos",
                *pw::keys::NODE_NAME => "aulos",
                *pw::keys::AUDIO_CHANNELS => "2",
                "node.rate" => format!("1/{rate}"),
                "node.latency" => format!("{}/{rate}", rate / 50),
            };
            if let Some(t) = &target {
                props.insert("target.object", t.as_str());
            }
            let stream = pw::stream::StreamBox::new(&core, "aulos-playback", props)?;
            // SAFETY (spike): the stream borrows `core`, which lives for the
            // whole thread; we never drop core before the stream.
            let stream: pw::stream::StreamBox<'static> = unsafe { std::mem::transmute(stream) };
            let ml = mainloop.clone();
            let ev2 = ev.clone();
            let listener = stream
                .add_local_listener_with_user_data(Rt { cons: cons.clone(), stats: stats.clone() })
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
                .drained(move |_, _| {
                    let _ = ev2.send(Event::Finished);
                    ml.quit();
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
    let _rx = cmd_rx.attach(mainloop.loop_(), move |cmd| match cmd {
        Cmd::Configure { rate, ack } => {
            // Drop the old stream first so the RT side releases the ring.
            live2.borrow_mut().take();
            match build(rate) {
                Ok(l) => *live2.borrow_mut() = Some(l),
                Err(e) => {
                    let _ = ev2.send(Event::Error(format!("stream: {e:#}")));
                }
            }
            let _ = ack.send(());
        }
        Cmd::Activate => {
            if let Some((s, _)) = live2.borrow().as_ref() {
                let _ = s.set_active(true);
            }
        }
        Cmd::Stop => {
            live2.borrow_mut().take();
            ml_stop.quit();
        }
        Cmd::Drain => {
            if let Some((s, _)) = live2.borrow().as_ref() {
                let _ = s.flush(true);
            }
        }
    });

    mainloop.run();
    live.borrow_mut().take();
    Ok(())
}
