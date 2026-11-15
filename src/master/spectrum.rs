//! The analyser behind the EQ display, run on the editor's thread from the audio thread's
//! ring of recent samples: a Hann-windowed FFT folded onto a log frequency axis, tilted so
//! pink noise reads flat, with meter-like ballistics.

use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};
use std::sync::Arc;

use super::shared::Ring;

pub const FFT: usize = 4096;
/// Points along the frequency axis, 20 Hz to 20 kHz.
pub const POINTS: usize = 160;

pub fn hz_of(i: usize) -> f32 {
    20.0 * 1000f32.powf(i as f32 / (POINTS - 1) as f32)
}

pub struct Analyser {
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    time: Vec<f32>,
    buf: Vec<Complex32>,
    /// Smoothed level per point, dB (pink noise is flat).
    pub db: [f32; POINTS],
}

impl Default for Analyser {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyser {
    pub fn new() -> Self {
        Self {
            fft: FftPlanner::new().plan_fft_forward(FFT),
            window: (0..FFT).map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / FFT as f32).cos()).collect(),
            time: vec![0.0; FFT],
            buf: vec![Complex32::default(); FFT],
            db: [-120.0; POINTS],
        }
    }

    /// Reads the newest samples and moves the display towards them. `dt` is the frame time.
    pub fn update(&mut self, ring: &Ring, fs: f32, dt: f32) {
        ring.read(&mut self.time);
        for (b, (x, w)) in self.buf.iter_mut().zip(self.time.iter().zip(&self.window)) {
            *b = Complex32::new(x * w, 0.0);
        }
        self.fft.process(&mut self.buf);
        let bin_hz = fs / FFT as f32;
        // A full-scale sine reads 0 dB: the Hann window's gain is 0.5, and it spreads a tone
        // over three bins that add up to 1.5 times its power.
        let norm = 2.0 / (FFT as f32 * 0.5) / 1.5f32.sqrt();
        let half = 2f32.powf(1.0 / 12.0);
        for i in 0..POINTS {
            let hz = hz_of(i);
            let lo = ((hz / half / bin_hz).floor() as usize).max(1);
            let hi = ((hz * half / bin_hz).ceil() as usize).clamp(lo + 1, FFT / 2);
            let mut power = 0.0f32;
            for b in &self.buf[lo.min(FFT / 2 - 1)..hi] {
                power += b.norm_sqr();
            }
            // Power per sixth of an octave, so pink noise comes out level across the display.
            let now = if hz < fs * 0.5 { 10.0 * (power * norm * norm).max(1e-14).log10() } else { -140.0 };
            let old = self.db[i];
            let rate = if now > old { 18.0 } else { 5.0 };
            self.db[i] = old + (now - old) * (1.0 - (-dt * rate).exp());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sine_shows_up_where_it_is() {
        let fs = 48_000.0;
        let ring = Ring::default();
        let x: Vec<f32> = (0..8192).map(|i| 0.5 * (std::f32::consts::TAU * 1000.0 * i as f32 / fs).sin()).collect();
        ring.write(&x);
        let mut a = Analyser::new();
        for _ in 0..200 {
            a.update(&ring, fs, 0.05);
        }
        let peak = (0..POINTS).max_by(|x, y| a.db[*x].total_cmp(&a.db[*y])).unwrap();
        assert!((hz_of(peak) / 1000.0).log2().abs() < 0.15, "{}", hz_of(peak));
        assert!((a.db[peak] - -6.0).abs() < 1.5, "{}", a.db[peak]);
    }
}
