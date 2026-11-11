//! Ten-band graphic EQ (canvas: 31 Hz .. 16 kHz, ±12 dB): RBJ peaking
//! biquads, one octave wide, run on the PipeWire RT thread. A preamp equal
//! to the cascade's peak boost (at any rate the engine runs) takes off the
//! steady-state boost, and a soft knee above 0.98 catches phase overshoot
//! instead of a hard clip. All zero = bypassed, so playback stays bit-perfect.

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

/// The coefficient sets of the bands that run at `rate`.
fn cascade(gains: &[f32; 10], rate: f64) -> Vec<[f64; 5]> {
    (0..10).filter(|&i| gains[i] != 0.0 && (BANDS[i] as f64) < 0.45 * rate).map(|i| peaking(BANDS[i] as f64, gains[i] as f64, rate)).collect()
}

/// The cascade's magnitude response in dB at `f`.
#[cfg(test)]
fn response_db(gains: &[f32; 10], f: f64, rate: f64) -> f64 {
    response_of(&cascade(gains, rate), f, rate)
}

fn response_of(cascade: &[[f64; 5]], f: f64, rate: f64) -> f64 {
    let w = 2.0 * std::f64::consts::PI * f / rate;
    let (c1, s1, c2, s2) = (w.cos(), w.sin(), (2.0 * w).cos(), (2.0 * w).sin());
    let mut db = 0.0;
    for &[b0, b1, b2, a1, a2] in cascade {
        // |B(e^-jw)| / |A(e^-jw)|
        let (br, bi) = (b0 + b1 * c1 + b2 * c2, -(b1 * s1 + b2 * s2));
        let (ar, ai) = (1.0 + a1 * c1 + a2 * c2, -(a1 * s1 + a2 * s2));
        db += 10.0 * ((br * br + bi * bi) / (ar * ar + ai * ai)).log10();
    }
    db
}

/// Rates the engine runs at (a stream matches its file's rate).
const RATES: [f64; 8] = [44_100.0, 48_000.0, 88_200.0, 96_000.0, 176_400.0, 192_000.0, 352_800.0, 384_000.0];

/// Linear preamp that cancels the cascade's highest boost at any of the
/// engine's rates (1.0 when it only cuts). Off the RT thread: ~8k evaluations.
pub fn preamp(gains: &[f32; 10]) -> f32 {
    let mut peak = 0.0f64;
    for &rate in &RATES {
        let c = cascade(gains, rate);
        if c.is_empty() {
            continue;
        }
        let top = 20_000f64.min(0.49 * rate);
        for i in 0..1000 {
            let f = 20.0 * (top / 20.0).powf(i as f64 / 999.0);
            peak = peak.max(response_of(&c, f, rate));
        }
    }
    10f64.powf(-peak / 20.0) as f32
}

/// Under 0.98 untouched; above, bend smoothly toward 1.0 (phase shifts can
/// push a waveform's peak past what the steady-state preamp covers).
#[inline]
fn soft_clip(x: f64) -> f64 {
    const KNEE: f64 = 0.98;
    let a = x.abs();
    if a <= KNEE { x } else { x.signum() * (KNEE + (1.0 - KNEE) * ((a - KNEE) / (1.0 - KNEE)).tanh()) }
}

/// Per-stream filter state, owned by the RT callback. No allocation.
pub struct Eq {
    gen_seen: u64,
    rate: u32,
    active: bool,
    /// Preamp now and its target: ramped over ~10 ms so a change doesn't click.
    pre: f64,
    pre_target: f64,
    pre_step: f64,
    coef: [[f64; 5]; 10],
    on: [bool; 10],
    /// [channel][band] = (z1, z2), transposed direct form II.
    z: [[(f64, f64); 10]; 2],
}

impl Default for Eq {
    fn default() -> Self {
        Eq { gen_seen: u64::MAX, rate: 0, active: false, pre: 1.0, pre_target: 1.0, pre_step: 0.0, coef: [[0.0; 5]; 10], on: [false; 10], z: [[(0.0, 0.0); 10]; 2] }
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
        let was_active = self.active;
        self.active = gains.iter().any(|&g| g != 0.0) && rate > 0;
        self.pre_target = preamp as f64;
        if !was_active || self.pre_target < self.pre {
            // More attenuation (a bigger boost) applies at once with the new
            // filters; only easing back up ramps, so a boost never runs short.
            self.pre = self.pre_target;
        }
        self.pre_step = (self.pre_target - self.pre) / (0.01 * rate.max(1) as f64);
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
        if self.pre != self.pre_target && ch == 0 {
            self.pre += self.pre_step;
            if (self.pre_step > 0.0 && self.pre > self.pre_target) || (self.pre_step < 0.0 && self.pre < self.pre_target) {
                self.pre = self.pre_target;
            }
        }
        let mut y = x as f64 * self.pre;
        for ((zi, coef), on) in self.z[ch & 1].iter_mut().zip(&self.coef).zip(&self.on) {
            if !on {
                continue;
            }
            let [b0, b1, b2, a1, a2] = *coef;
            let (z1, z2) = *zi;
            let out = b0 * y + z1;
            let (n1, n2) = (b1 * y - a1 * out + z2, b2 * y - a2 * out);
            // Flush what decays toward denormals in silence (slow on some CPUs).
            *zi = (if n1.abs() < 1e-20 { 0.0 } else { n1 }, if n2.abs() < 1e-20 { 0.0 } else { n2 });
            y = out;
        }
        (soft_clip(y) as f32).clamp(-1.0, 1.0)
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
        // Treble at 192 kHz: the preamp covers the real peak there.
        let tr = [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 3.0, 4.5, 5.5];
        let worst = (0..2000).map(|i| 20.0 * 1000f64.powf(i as f64 / 1999.0)).map(|f| response_db(&tr, f, 192_000.0)).fold(0.0f64, f64::max);
        assert!(20.0 * (preamp(&tr) as f64).log10() <= -worst + 1e-6);
        // Silence after an impulse drains to exact zero (no denormal state).
        let mut g = [0.0; 10];
        g[9] = 12.0;
        let mut e = Eq::default();
        e.update(1, 48_000, &g, preamp(&g));
        e.process(0, 1.0);
        for _ in 0..48_000 {
            e.process(0, 0.0);
        }
        assert!(e.z[0].iter().all(|&(a, b)| a == 0.0 && b == 0.0));
        // A cut-only square wave can overshoot; the knee keeps it under 1.0.
        let mut g = [0.0; 10];
        g[5] = -12.0;
        let mut e = Eq::default();
        e.update(1, 48_000, &g, preamp(&g));
        let mut top = 0.0f32;
        for n in 0..48_000 {
            let x = if (n / 240) % 2 == 0 { 0.99 } else { -0.99 };
            top = top.max(e.process(0, x).abs());
        }
        assert!(top < 1.0, "{top}");
    }
}
