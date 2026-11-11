//! Crate audio spike: decode local files back to back with symphonia, push
//! f32 frames through a lock-free ring, play them on a PipeWire stream at the
//! source's native rate. Writes the exact samples it queued to a reference
//! file so a recording of the output can be compared bit for bit.
//!
//! usage: audio_spike [--target NODE] [--limit SECS] [--ref out.f32] FILE...

use std::{
    fs::File,
    io::{BufWriter, Write},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
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

const CHANNELS: usize = 2;

struct Stats {
    frames_played: AtomicU64,
    underrun_callbacks: AtomicU64,
    underrun_frames: AtomicU64,
    callbacks: AtomicU64,
    max_cb_ns: AtomicU64,
    decoder_done: AtomicBool,
    drained: AtomicBool,
}

struct Args {
    target: Option<String>,
    limit: Option<f64>,
    reference: Option<PathBuf>,
    files: Vec<PathBuf>,
}

fn parse_args() -> Args {
    let mut a = Args { target: None, limit: None, reference: None, files: vec![] };
    let mut it = std::env::args().skip(1);
    while let Some(s) = it.next() {
        match s.as_str() {
            "--target" => a.target = it.next(),
            "--limit" => a.limit = it.next().and_then(|v| v.parse().ok()),
            "--ref" => a.reference = it.next().map(PathBuf::from),
            _ => a.files.push(PathBuf::from(s)),
        }
    }
    assert!(!a.files.is_empty(), "no input files");
    a
}

/// Probe a file's sample rate without decoding.
fn probe_rate(path: &PathBuf) -> anyhow::Result<u32> {
    let mss = MediaSourceStream::new(Box::new(File::open(path)?), Default::default());
    let fmt = symphonia::default::get_probe().probe(
        &Hint::new(),
        mss,
        FormatOptions::default(),
        MetadataOptions::default(),
    )?;
    let track = fmt.default_track(TrackType::Audio).ok_or_else(|| anyhow::anyhow!("no audio track"))?;
    match &track.codec_params {
        Some(CodecParameters::Audio(p)) => p.sample_rate.ok_or_else(|| anyhow::anyhow!("no rate")),
        _ => anyhow::bail!("not audio"),
    }
}

fn decode_all(
    files: Vec<PathBuf>,
    rate: u32,
    limit_frames: Option<u64>,
    mut prod: rtrb::Producer<f32>,
    mut reference: Option<BufWriter<File>>,
    stats: Arc<Stats>,
) -> anyhow::Result<()> {
    let mut queued: u64 = 0;
    let mut scratch: Vec<f32> = Vec::new();
    'files: for path in files {
        let t0 = Instant::now();
        let mss = MediaSourceStream::new(Box::new(File::open(&path)?), Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }
        let mut fmt = symphonia::default::get_probe().probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )?;
        let track = fmt.default_track(TrackType::Audio).unwrap().clone();
        let Some(CodecParameters::Audio(params)) = &track.codec_params else { anyhow::bail!("not audio") };
        let file_rate = params.sample_rate.unwrap_or(0);
        anyhow::ensure!(
            file_rate == rate,
            "{}: rate {} != stream rate {} (rate change between tracks needs a stream reconnect)",
            path.display(),
            file_rate,
            rate
        );
        eprintln!(
            "[decode] {} · {:?} · {} Hz · {} bit",
            path.file_name().unwrap().to_string_lossy(),
            params.codec,
            file_rate,
            params.bits_per_sample.unwrap_or(0)
        );
        let mut dec = symphonia::default::get_codecs()
            .make_audio_decoder(params, &AudioDecoderOptions::default().gapless(true))?;
        let mut file_frames = 0u64;
        while let Some(packet) = fmt.next_packet()? {
            if packet.track_id != track.id {
                continue;
            }
            let buf = dec.decode(&packet)?;
            let ch = buf.spec().channels().count();
            anyhow::ensure!(ch == CHANNELS, "only stereo in the spike (got {ch})");
            let n = buf.frames() * ch;
            scratch.resize(n, 0.0);
            buf.copy_to_slice_interleaved(&mut scratch[..]);
            let mut samples = &scratch[..];
            if let Some(lim) = limit_frames {
                let left = lim.saturating_sub(queued) as usize * CHANNELS;
                if left == 0 {
                    break 'files;
                }
                samples = &samples[..samples.len().min(left)];
            }
            if let Some(w) = reference.as_mut() {
                for s in samples {
                    w.write_all(&s.to_le_bytes())?;
                }
            }
            // Push into the ring; block (sleep) while it's full.
            let mut off = 0;
            while off < samples.len() {
                let free = prod.slots();
                if free == 0 {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                let take = free.min(samples.len() - off);
                let chunk = prod.write_chunk_uninit(take).unwrap();
                chunk.fill_from_iter(samples[off..off + take].iter().copied());
                off += take;
            }
            queued += (samples.len() / CHANNELS) as u64;
            file_frames += (samples.len() / CHANNELS) as u64;
        }
        eprintln!(
            "[decode] done: {} frames ({:.2} s) decoded in {:?} wall (ring-paced)",
            file_frames,
            file_frames as f64 / rate as f64,
            t0.elapsed()
        );
    }
    if let Some(mut w) = reference {
        w.flush()?;
    }
    stats.decoder_done.store(true, Ordering::Release);
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let args = parse_args();
    let rate = probe_rate(&args.files[0])?;
    eprintln!("[crate-audio] stream rate {rate} Hz, {} file(s)", args.files.len());

    let stats = Arc::new(Stats {
        frames_played: AtomicU64::new(0),
        underrun_callbacks: AtomicU64::new(0),
        underrun_frames: AtomicU64::new(0),
        callbacks: AtomicU64::new(0),
        max_cb_ns: AtomicU64::new(0),
        decoder_done: AtomicBool::new(false),
        drained: AtomicBool::new(false),
    });

    // Two seconds of headroom between decoder and audio thread.
    let (prod, cons) = rtrb::RingBuffer::<f32>::new(rate as usize * CHANNELS * 2);
    let reference = args.reference.as_ref().map(|p| BufWriter::new(File::create(p).unwrap()));
    let limit_frames = args.limit.map(|s| (s * rate as f64) as u64);
    {
        let stats = stats.clone();
        let files = args.files.clone();
        std::thread::Builder::new().name("decoder".into()).spawn(move || {
            if let Err(e) = decode_all(files, rate, limit_frames, prod, reference, stats.clone()) {
                eprintln!("[decode] error: {e:#}");
                stats.decoder_done.store(true, Ordering::Release);
            }
        })?;
    }
    // Let the decoder get ahead before audio starts pulling.
    std::thread::sleep(Duration::from_millis(300));

    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;

    let mut props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_ROLE => "Music",
        *pw::keys::MEDIA_CATEGORY => "Playback",
        *pw::keys::APP_NAME => "Crate",
        *pw::keys::NODE_NAME => "crate",
        *pw::keys::AUDIO_CHANNELS => "2",
        // Ask the graph to run at the source rate (honoured only if allowed-rates permits).
        "node.rate" => format!("1/{rate}"),
        "node.latency" => format!("{}/{rate}", rate / 50),
    };
    if let Some(t) = &args.target {
        props.insert("target.object", t.as_str());
    }
    let stream = pw::stream::StreamBox::new(&core, "crate-playback", props)?;

    struct Rt {
        cons: rtrb::Consumer<f32>,
        stats: Arc<Stats>,
    }
    let rt = Rt { cons, stats: stats.clone() };

    let _listener = stream
        .add_local_listener_with_user_data(rt)
        .process(|stream, rt| {
            // Realtime thread: no allocation, no locks, no syscalls.
            let t0 = Instant::now();
            let Some(mut buffer) = stream.dequeue_buffer() else { return };
            let stride = 4 * CHANNELS;
            let datas = buffer.datas_mut();
            let data = &mut datas[0];
            let mut written = 0usize;
            if let Some(slice) = data.data() {
                let cap_frames = slice.len() / stride;
                let avail = rt.cons.slots() / CHANNELS;
                let frames = cap_frames.min(avail);
                if frames > 0 {
                    let chunk = rt.cons.read_chunk(frames * CHANNELS).unwrap();
                    let (a, b) = chunk.as_slices();
                    for (i, s) in a.iter().chain(b.iter()).enumerate() {
                        slice[i * 4..i * 4 + 4].copy_from_slice(&s.to_le_bytes());
                    }
                    chunk.commit_all();
                }
                if frames < cap_frames {
                    slice[frames * stride..cap_frames * stride].fill(0);
                    if rt.stats.decoder_done.load(Ordering::Acquire) {
                        rt.stats.drained.store(true, Ordering::Release);
                    } else {
                        rt.stats.underrun_callbacks.fetch_add(1, Ordering::Relaxed);
                        rt.stats.underrun_frames.fetch_add((cap_frames - frames) as u64, Ordering::Relaxed);
                    }
                }
                rt.stats.frames_played.fetch_add(frames as u64, Ordering::Relaxed);
                written = cap_frames;
            }
            let chunk = data.chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.stride_mut() = stride as _;
            *chunk.size_mut() = (stride * written) as _;
            rt.stats.callbacks.fetch_add(1, Ordering::Relaxed);
            let ns = t0.elapsed().as_nanos() as u64;
            rt.stats.max_cb_ns.fetch_max(ns, Ordering::Relaxed);
        })
        .register()?;

    let mut info = spa::param::audio::AudioInfoRaw::new();
    info.set_format(spa::param::audio::AudioFormat::F32LE);
    info.set_rate(rate);
    info.set_channels(CHANNELS as u32);
    let mut position = [0; spa::param::audio::MAX_CHANNELS];
    position[0] = spa_sys::SPA_AUDIO_CHANNEL_FL;
    position[1] = spa_sys::SPA_AUDIO_CHANNEL_FR;
    info.set_position(position);
    let values: Vec<u8> = pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pw::spa::pod::Value::Object(pw::spa::pod::Object {
            type_: spa_sys::SPA_TYPE_OBJECT_Format,
            id: spa_sys::SPA_PARAM_EnumFormat,
            properties: info.into(),
        }),
    )
    .unwrap()
    .0
    .into_inner();
    let mut params = [Pod::from_bytes(&values).unwrap()];
    stream.connect(
        spa::utils::Direction::Output,
        None,
        pw::stream::StreamFlags::AUTOCONNECT
            | pw::stream::StreamFlags::MAP_BUFFERS
            | pw::stream::StreamFlags::RT_PROCESS,
        &mut params,
    )?;

    // Poll for completion on the main loop (never from the RT thread).
    let started = Instant::now();
    let ml = mainloop.clone();
    let st = stats.clone();
    // Once the ring runs dry, give the graph a few cycles to play what's queued.
    let ticks_after_drain = std::cell::Cell::new(0u32);
    let timer = mainloop.loop_().add_timer(move |_| {
        if st.drained.load(Ordering::Acquire) {
            ticks_after_drain.set(ticks_after_drain.get() + 1);
            if ticks_after_drain.get() >= 3 {
                ml.quit();
            }
        }
    });
    timer.update_timer(Some(Duration::from_millis(100)), Some(Duration::from_millis(100))).into_result()?;

    mainloop.run();

    let s = &stats;
    let played = s.frames_played.load(Ordering::Relaxed);
    println!(
        "AUDIO rate={} played_frames={} ({:.2} s) wall={:.2} s callbacks={} underrun_callbacks={} underrun_frames={} max_callback_us={:.1}",
        rate,
        played,
        played as f64 / rate as f64,
        started.elapsed().as_secs_f64(),
        s.callbacks.load(Ordering::Relaxed),
        s.underrun_callbacks.load(Ordering::Relaxed),
        s.underrun_frames.load(Ordering::Relaxed),
        s.max_cb_ns.load(Ordering::Relaxed) as f64 / 1000.0
    );
    Ok(())
}
