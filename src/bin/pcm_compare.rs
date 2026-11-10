//! Compare a recording (raw f32le stereo) against the reference PCM the
//! player queued. Aligns on the first stretch of matching samples, then counts
//! exact mismatches and reports the first divergence.
//!
//! usage: pcm_compare reference.f32 recording.f32

fn read(path: &str) -> Vec<f32> {
    let b = std::fs::read(path).expect(path);
    b.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect()
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let reference = read(&a[1]);
    let rec = read(&a[2]);
    println!("reference {} samples, recording {} samples", reference.len(), rec.len());

    // Anchor: first window of the reference that isn't silence.
    const W: usize = 256;
    let start = reference
        .windows(W)
        .position(|w| w.iter().filter(|s| **s != 0.0).count() > W / 2)
        .expect("reference is silent");
    let anchor = &reference[start..start + W];
    let Some(pos) = rec.windows(W).position(|w| w == anchor) else {
        // Not bit-identical anywhere: measure how close it is.
        let peak = |v: &[f32]| v.iter().fold(0f32, |m, s| m.max(s.abs()));
        println!(
            "RESULT no exact alignment found (not bit-perfect). ref peak {:.6}, rec peak {:.6}",
            peak(&reference),
            peak(&rec)
        );
        return;
    };
    let offset = pos as isize - start as isize;
    println!("aligned: recording offset {} samples", offset);

    let mut mismatches = 0usize;
    let mut first: Option<usize> = None;
    let mut compared = 0usize;
    for (i, r) in reference.iter().enumerate() {
        let j = i as isize + offset;
        if j < 0 || j as usize >= rec.len() {
            continue;
        }
        compared += 1;
        if rec[j as usize] != *r {
            mismatches += 1;
            first.get_or_insert(i);
        }
    }
    println!(
        "RESULT compared {} samples ({} stereo frames), mismatches {}, first mismatch at ref sample {:?}",
        compared,
        compared / 2,
        mismatches,
        first
    );
}
