//! Drive the engine from the command line and report the signal path.
//!
//! usage: player [--target NODE] ITEM...
//!   ITEM = a local file, or plex:/library/parts/<id>/<ts>/file.<ext>
//!   (plex items stream progressively into /scratch/aulos-cache; needs PLEX_TOKEN)

use std::{path::PathBuf, sync::atomic::Ordering, time::Duration};

use aulos_spike::audio::{Engine, Event, Source};

fn main() -> anyhow::Result<()> {
    let mut target = None;
    let mut queue = Vec::new();
    let base = std::env::var("PLEX_URL").unwrap_or_else(|_| "http://plex-server:32400".into());
    let cache_dir = PathBuf::from(
        std::env::var("AULOS_CACHE").unwrap_or_else(|_| "/scratch/aulos-cache".into()),
    );
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        if a == "--target" {
            target = it.next();
        } else if let Some(path) = a.strip_prefix("plex:") {
            let token = std::env::var("PLEX_TOKEN").expect("PLEX_TOKEN for plex: items");
            let parts: Vec<&str> = path.split('/').collect();
            let name = format!("part-{}.{}", parts.get(3).unwrap_or(&"x"), path.rsplit('.').next().unwrap());
            queue.push(Source::Http { url: format!("{base}{path}"), token, cache: cache_dir.join(name) });
        } else {
            queue.push(Source::File(PathBuf::from(a)));
        }
    }
    anyhow::ensure!(!queue.is_empty(), "nothing to play");

    let engine = Engine::play(queue, target)?;
    let st = engine.stats.clone();
    let mut tracks: Vec<(u64, u32, String)> = Vec::new();
    let mut reported = 0usize;
    loop {
        match engine.events.recv_timeout(Duration::from_millis(250)) {
            Ok(Event::TrackStarted { index, label, source_rate, bits, at_frame, .. }) => {
                eprintln!("[queue] #{index} {label}: {source_rate} Hz / {bits}-bit, starts at frame {at_frame}");
                tracks.push((at_frame, source_rate, label));
            }
            Ok(Event::Reconnected { from, to, silent_ms }) => {
                println!("RECONNECT {from} -> {to} Hz: {silent_ms:.1} ms of silence between tracks");
            }
            Ok(Event::FirstAudio { ms_since_play }) => println!("FIRST_AUDIO {ms_since_play:.0} ms after play"),
            Ok(Event::Error(e)) => {
                eprintln!("ERROR {e}");
                break;
            }
            Ok(Event::Finished) => break,
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
