//! Drive the engine from the command line and report the signal path.
//!
//! usage: player [--target NODE] ITEM...
//!   ITEM = a local file, or plex:/library/parts/<id>/<ts>/file.<ext>
//!   (plex items stream progressively into $CRATE_AUDIO_CACHE, else
//!   else ~/.cache/crate/audio; needs PLEX_URL and PLEX_TOKEN)

use std::{path::PathBuf, sync::atomic::Ordering, time::Duration};

use crate_audio::audio::{Engine, Event, Source};

fn main() -> anyhow::Result<()> {
    let mut target = None;
    let mut queue = Vec::new();
    let base = std::env::var("PLEX_URL").unwrap_or_default();
    // The app's default (plex::audio_cache_dir, minus its config.toml option).
    let cache_dir = match std::env::var_os("CRATE_AUDIO_CACHE").filter(|v| !v.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".cache"))
            .join("crate/audio"),
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        if a == "--target" {
            target = it.next();
        } else if let Some(path) = a.strip_prefix("plex:") {
            let token = std::env::var("PLEX_TOKEN").expect("PLEX_TOKEN for plex: items");
            anyhow::ensure!(!base.is_empty(), "PLEX_URL for plex: items (e.g. http://192.168.1.10:32400)");
            let parts: Vec<&str> = path.split('/').collect();
            let name = format!("part-{}.{}", parts.get(3).unwrap_or(&"x"), path.rsplit('.').next().unwrap());
            queue.push(Source::Http { url: format!("{base}{path}"), token, cache: cache_dir.join(name), prepare: None, len_ms: None, start_ms: 0 });
        } else {
            queue.push(Source::File(PathBuf::from(a)));
        }
    }
    anyhow::ensure!(!queue.is_empty(), "nothing to play");

    // Test aid: CRATE_FADE_MS crossfades into every item after the first.
    let fade_ms: u32 = std::env::var("CRATE_FADE_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    let items = queue
        .into_iter()
        .enumerate()
        .map(|(i, src)| crate_audio::audio::Item { src, gain: 1.0, fade_ms: if i > 0 { fade_ms } else { 0 } })
        .collect();
    let engine = Engine::play(items, target)?;
    // Test aid: CRATE_EQ="g31,g62,...,g16k" (dB) runs the ten-band EQ.
    if let Ok(v) = std::env::var("CRATE_EQ") {
        let mut g = [0.0f32; 10];
        for (slot, x) in g.iter_mut().zip(v.split(',')) {
            *slot = x.trim().parse().unwrap_or(0.0);
        }
        engine.set_eq(g);
    }
    let st = engine.stats.clone();
    let mut tracks: Vec<(u64, u32, String)> = Vec::new();
    let mut reported = 0usize;
    // Stress aid: pause/resume every N ms (exercises set_active during drains).
    let toggle_ms: Option<u64> = std::env::var("CRATE_TOGGLE_MS").ok().and_then(|v| v.parse().ok());
    let mut toggled_off = false;
    let mut toggles = 0u64;
    let toggle_for = std::env::var("CRATE_TOGGLE_FOR_S").ok().and_then(|v| v.parse::<f64>().ok());
    let started = std::time::Instant::now();
    loop {
        let toggling = toggle_ms.is_some() && toggle_for.is_none_or(|f| started.elapsed().as_secs_f64() < f);
        if !toggling && toggled_off {
            toggled_off = false;
            engine.set_active(true);
            eprintln!("[stress] toggling stopped after {toggles} toggles, resumed");
        }
        if toggling {
            toggled_off = !toggled_off;
            engine.set_active(!toggled_off);
            toggles += 1;
        }
        match engine.events.recv_timeout(Duration::from_millis(toggle_ms.unwrap_or(250))) {
            Ok(Event::TrackStarted { index, label, source_rate, bits, at_frame, .. }) => {
                eprintln!("[queue] #{index} {label}: {source_rate} Hz / {bits}-bit, starts at frame {at_frame}");
                tracks.push((at_frame, source_rate, label));
            }
            Ok(Event::Skipped { index, error, .. }) => eprintln!("[queue] #{index} skipped: {error}"),
            Ok(Event::Reconnected { from, to, silent_ms }) => {
                println!("RECONNECT {from} -> {to} Hz: {silent_ms:.1} ms of silence between tracks");
            }
            Ok(Event::FirstAudio { ms_since_play }) => println!("FIRST_AUDIO {ms_since_play:.0} ms after play"),
            Ok(Event::Error(e)) => {
                eprintln!("ERROR {e}");
                break;
            }
            Ok(Event::Finished) => {
                if toggle_ms.is_some() {
                    println!("TOGGLES {toggles}");
                }
                break;
            }
            Err(_) => {}
        }
        // Signal-path readback once each track has been playing ~2 s.
        // (frames_played spans rate changes; good enough to find the current track.)
        while reported < tracks.len() {
            let (start, src_rate, label) = &tracks[reported];
            let played = st.frames_played.load(Ordering::Relaxed);
            if played < start + *src_rate as u64 * 2 {
                break;
            }
            let graph = st.graph_rate.load(Ordering::Relaxed);
            let verdict = if graph == *src_rate {
                "native rate end to end (bit-perfect possible)".to_string()
            } else {
                format!("RESAMPLED by PipeWire {src_rate} -> {graph} Hz")
            };
            println!("SIGNAL #{reported} {label}: source {src_rate} Hz, graph {graph} Hz: {verdict}");
            reported += 1;
        }
    }
    println!(
        "DONE played {:.2} s, callbacks {}, underruns {}, max callback {:.1} us",
        st.played_us.load(Ordering::Relaxed) as f64 / 1e6,
        st.callbacks.load(Ordering::Relaxed),
        st.underrun_callbacks.load(Ordering::Relaxed),
        st.max_cb_ns.load(Ordering::Relaxed) as f64 / 1000.0
    );
    Ok(())
}
