//! Lock-free state between Persona Master's audio thread and its editor: meters, the
//! analyser feeds, level-matched compare, and the Auto Master handshake.

use nih_plug::prelude::AtomicF32;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use super::params::{BAL_BANDS, MB_BANDS};

pub const LEARN_IDLE: u32 = 0;
/// Listening to the mix (Auto Master, first pass).
pub const LEARN_LISTENING: u32 = 1;
/// The first pass finished; the editor applies the result and asks for a refine pass.
pub const LEARN_HEARD: u32 = 2;
/// Listening to the output to fine-tune the loudness.
pub const LEARN_REFINING: u32 = 3;
pub const LEARN_REFINED: u32 = 4;
pub const LEARN_FAILED: u32 = 5;

pub const REQ_NONE: u32 = 0;
pub const REQ_LISTEN: u32 = 1;
pub const REQ_REFINE: u32 = 2;

/// Samples kept for the analyser.
pub const RING: usize = 8192;

/// A ring of recent samples the editor can read without locking.
pub struct Ring {
    buf: Box<[AtomicF32]>,
    pub w: AtomicUsize,
}

impl Default for Ring {
    fn default() -> Self {
        Self { buf: (0..RING).map(|_| AtomicF32::new(0.0)).collect(), w: AtomicUsize::new(0) }
    }
}

impl Ring {
    /// Writes a block (the write index is published once at the end).
    pub fn write(&self, x: &[f32]) {
        let mut w = self.w.load(Ordering::Relaxed);
        for v in x {
            self.buf[w % RING].store(*v, Ordering::Relaxed);
            w = w.wrapping_add(1);
        }
        self.w.store(w, Ordering::Release);
    }

    /// Copies the newest `out.len()` samples, oldest first.
    pub fn read(&self, out: &mut [f32]) {
        let w = self.w.load(Ordering::Acquire);
        let n = out.len().min(RING);
        for (i, o) in out.iter_mut().take(n).enumerate() {
            *o = self.buf[(w.wrapping_sub(n) + i) % RING].load(Ordering::Relaxed);
        }
    }
}

/// What Auto Master heard.
pub struct Heard {
    /// Integrated loudness of the mix where Auto Master listens (after the EQ), LUFS.
    pub lufs: AtomicF32,
    pub true_peak_db: AtomicF32,
    pub range: AtomicF32,
    /// Long-term level per Balance band, dB (pink noise reads flat).
    pub bands_db: [AtomicF32; BAL_BANDS],
    /// Level the loud moments reach (90th percentile) in each multiband band, as the band
    /// compressors measure it, dBFS.
    pub mb_db: [AtomicF32; MB_BANDS],
    /// The same for the whole mix as the Glue compressor measures it, dBFS.
    pub glue_db: AtomicF32,
    /// Side level over mid level (0 = mono, 1 = as much side as mid), whole band and below
    /// 120 Hz.
    pub side_ratio: AtomicF32,
    pub low_side_ratio: AtomicF32,
    /// Integrated loudness of the output during the refine pass, LUFS.
    pub out_lufs: AtomicF32,
}

pub struct MasterShared {
    pub sample_rate: AtomicF32,
    pub latency: AtomicUsize,

    pub in_peak: [AtomicF32; 2],
    pub out_peak: [AtomicF32; 2],
    pub momentary: AtomicF32,
    pub short_term: AtomicF32,
    pub integrated: AtomicF32,
    pub range: AtomicF32,
    pub true_peak: AtomicF32,
    pub in_short_term: AtomicF32,
    pub glue_gr: AtomicF32,
    pub mb_gr: [AtomicF32; MB_BANDS],
    pub max_gr: AtomicF32,
    pub correlation: AtomicF32,
    pub width: AtomicF32,

    /// Gain match: the master is turned down to the loudness of the mix going in, so
    /// bypassing compares the sound and not the volume. A monitoring aid: never saved, and it
    /// must be off when the song is exported.
    pub gain_match: AtomicBool,
    /// What gain match is taking off right now, dB (<= 0).
    pub match_db: AtomicF32,
    /// What the soft clipper is shaving off the peaks, dB.
    pub clip_gr: AtomicF32,
    /// Set by the editor to start the integrated loudness and true-peak hold over.
    pub reset_meters: AtomicBool,

    pub spec_in: Ring,
    pub spec_out: Ring,
    /// The output's left and right channels, for the vectorscope.
    pub scope: [Ring; 2],

    pub learn_request: AtomicU32,
    pub learn_cancel: AtomicBool,
    pub learn_state: AtomicU32,
    pub learn_progress: AtomicF32,
    pub heard: Heard,
}

fn f(v: f32) -> AtomicF32 {
    AtomicF32::new(v)
}

impl Default for MasterShared {
    fn default() -> Self {
        Self {
            sample_rate: f(48_000.0),
            latency: AtomicUsize::new(0),
            in_peak: [f(0.0), f(0.0)],
            out_peak: [f(0.0), f(0.0)],
            momentary: f(-100.0),
            short_term: f(-100.0),
            integrated: f(-100.0),
            range: f(0.0),
            true_peak: f(-100.0),
            in_short_term: f(-100.0),
            glue_gr: f(0.0),
            mb_gr: std::array::from_fn(|_| f(0.0)),
            max_gr: f(0.0),
            correlation: f(1.0),
            width: f(0.0),
            gain_match: AtomicBool::new(false),
            match_db: f(0.0),
            clip_gr: f(0.0),
            reset_meters: AtomicBool::new(false),
            spec_in: Ring::default(),
            spec_out: Ring::default(),
            scope: [Ring::default(), Ring::default()],
            learn_request: AtomicU32::new(REQ_NONE),
            learn_cancel: AtomicBool::new(false),
            learn_state: AtomicU32::new(LEARN_IDLE),
            learn_progress: f(0.0),
            heard: Heard {
                lufs: f(-100.0),
                true_peak_db: f(-100.0),
                range: f(0.0),
                bands_db: std::array::from_fn(|_| f(-100.0)),
                mb_db: std::array::from_fn(|_| f(-100.0)),
                glue_db: f(-100.0),
                side_ratio: f(0.0),
                low_side_ratio: f(0.0),
                out_lufs: f(-100.0),
            },
        }
    }
}

impl MasterShared {
    /// Rises at once, falls slowly.
    pub fn meter(slot: &AtomicF32, value: f32) {
        let old = slot.load(Ordering::Relaxed);
        slot.store(if value > old { value } else { old * 0.9 + value * 0.1 }, Ordering::Relaxed);
    }
}
