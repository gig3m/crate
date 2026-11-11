//! Ten-band graphic EQ (canvas: 31 Hz .. 16 kHz, ±12 dB): RBJ peaking
//! biquads, one octave wide, run on the PipeWire RT thread. A preamp equal
//! to the cascade's peak boost keeps it from clipping. All zero = bypassed,
//! so playback stays bit-perfect.

pub const BANDS: [f32; 10] = [31.0, 62.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0];
const Q: f64 = std::f64::consts::SQRT_2;

/// Normalized biquad coefficients [b0, b1, b2, a1, a2] for a peaking band.
pub fn peaking(f: f64, gain_db: f64, rate: f64) -> [f64; 5] {
    let a = 10f64.powf(gain_db / 40.0);
    let w0 = 2.0 * std::f64::consts::PI * f / rate;
    let alpha = w0.sin() / (2.0 * Q);
    let cos = w0.cos();
    let a0 = 1.0 + alpha / a;
    [(1.0 + alpha * a) / a0, -2.0 * cos / a0, (1.0 - alpha * a) / a0, -2.0 * cos / a0, (1.0 - alpha / a) / a0]
}

/// The cascade's magnitude response in dB at `f`.
fn response_db(gains: &[f32; 10], f: f64, rate: f64) -> f64 {
    let w = 2.0 * std::f64::consts::PI * f / rate;
    let (c1, s1, c2, s2) = (w.cos(), w.sin(), (2.0 * w).cos(), (2.0 * w).sin());
    let mut db = 0.0;
    for (i, &g) in gains.iter().enumerate() {
        if g == 0.0 || BANDS[i] as f64 >= 0.45 * rate {
            continue;
        }
        let [b0, b1, b2, a1, a2] = peaking(BANDS[i] as f64, g as f64, rate);
        // |B(e^-jw)| / |A(e^-jw)|
        let (br, bi) = (b0 + b1 * c1 + b2 * c2, -(b1 * s1 + b2 * s2));
        let (ar, ai) = (1.0 + a1 * c1 + a2 * c2, -(a1 * s1 + a2 * s2));
        db += 10.0 * ((br * br + bi * bi) / (ar * ar + ai * ai)).log10();
    }
    db
}

/// Linear preamp that cancels the cascade's highest boost (1.0 when it only cuts).
pub fn preamp(gains: &[f32; 10]) -> f32 {
    let peak = (0..240).map(|i| 20.0 * 1000f64.powf(i as f64 / 239.0)).map(|f| response_db(gains, f, 48_000.0)).fold(0.0f64, f64::max);
    10f64.powf(-peak / 20.0) as f32
}

/// Per-stream filter state, owned by the RT callback. No allocation.
pub struct Eq {
    gen_seen: u64,
    rate: u32,
    active: bool,
    pre: f64,
    coef: [[f64; 5]; 10],
    on: [bool; 10],
    /// [channel][band] = (z1, z2), transposed direct form II.
    z: [[(f64, f64); 10]; 2],
}

impl Default for Eq {
    fn default() -> Self {
        Eq { gen_seen: u64::MAX, rate: 0, active: false, pre: 1.0, coef: [[0.0; 5]; 10], on: [false; 10], z: [[(0.0, 0.0); 10]; 2] }
    }
}

impl Eq {
    /// Recompute when the settings (`gen`) or the stream rate changed.
    pub fn update(&mut self, gen_id: u64, rate: u32, gains: &[f32; 10], preamp: f32) {
        if gen_id == self.gen_seen && rate == self.rate {
            return;
        }
        if rate != self.rate {
            self.z = [[(0.0, 0.0); 10]; 2];
        }
        self.gen_seen = gen_id;
        self.rate = rate;
        self.active = gains.iter().any(|&g| g != 0.0) && rate > 0;
        self.pre = preamp as f64;
        for i in 0..10 {
            self.on[i] = gains[i] != 0.0 && (BANDS[i] as f64) < 0.45 * rate as f64;
            if self.on[i] {
                self.coef[i] = peaking(BANDS[i] as f64, gains[i] as f64, rate as f64);
            } else {
                self.z[0][i] = (0.0, 0.0);
                self.z[1][i] = (0.0, 0.0);
            }
        }
    }

    pub fn active(&self) -> bool {
        self.active
    }

    /// Filter one sample on channel `ch` (0 or 1).
    #[inline]
    pub fn process(&mut self, ch: usize, x: f32) -> f32 {
        let mut y = x as f64 * self.pre;
        for ((zi, coef), on) in self.z[ch & 1].iter_mut().zip(&self.coef).zip(&self.on) {
            if !on {
                continue;
            }
            let [b0, b1, b2, a1, a2] = *coef;
            let (z1, z2) = *zi;
            let out = b0 * y + z1;
            *zi = (b1 * y - a1 * out + z2, b2 * y - a2 * out);
            y = out;
        }
        (y as f32).clamp(-1.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eq_math() {
        // Flat: no preamp, inactive.
        let flat = [0.0; 10];
        assert_eq!(preamp(&flat), 1.0);
        let mut e = Eq::default();
        e.update(1, 48_000, &flat, 1.0);
        assert!(!e.active());
        // A +6 dB band peaks at ~+6 dB at its centre; preamp cancels it.
        let mut g = [0.0; 10];
        g[5] = 6.0;
        assert!((response_db(&g, 1000.0, 48_000.0) - 6.0).abs() < 0.05);
        assert!((preamp(&g) - 10f32.powf(-6.0 / 20.0)).abs() < 0.01);
        // Cuts only: preamp stays 1.
        assert_eq!(preamp(&[-3.0; 10]), 1.0);
        // A 1 kHz sine through +6 dB at 1 kHz with its preamp comes out at ~unity.
        let mut e = Eq::default();
        e.update(1, 48_000, &g, preamp(&g));
        let mut peak = 0.0f32;
        for n in 0..48_000 {
            let x = 0.5 * (2.0 * std::f32::consts::PI * 1000.0 * n as f32 / 48_000.0).sin();
            let y = e.process(0, x);
            if n > 24_000 {
                peak = peak.max(y.abs());
            }
        }
        assert!((peak - 0.5).abs() < 0.01, "{peak}");
    }
}
