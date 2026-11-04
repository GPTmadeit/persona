//! Lock-free state between the audio thread and the editor: meters, what the tuner hears, and
//! the Match My Voice handshake.

use nih_plug::prelude::AtomicF32;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicUsize, Ordering};

use crate::params::MATCH_BANDS;

/// Match My Voice states, in `Shared::learn_state`.
pub const LEARN_IDLE: u32 = 0;
pub const LEARN_LISTENING: u32 = 1;
pub const LEARN_DONE: u32 = 2;
pub const LEARN_FAILED: u32 = 3;

/// What Match My Voice measured, published once when it finishes.
pub struct LearnResult {
    /// Average level of the voiced input, dBFS RMS, before the Input knob.
    pub level_db: AtomicF32,
    /// Long-term spectrum of the voice per Match band, dB.
    pub bands_db: [AtomicF32; MATCH_BANDS],
    /// Loudest sibilance (95th percentile), dBFS, measured where the de-esser listens.
    pub sibilance_db: AtomicF32,
    /// Median sung pitch, MIDI (0 if the take had no clear pitch).
    pub median_midi: AtomicF32,
    pub key_root: AtomicI32,
    pub key_minor: AtomicBool,
    pub key_valid: AtomicBool,
    pub key_confidence: AtomicF32,
}

pub struct Shared {
    pub sample_rate: AtomicF32,
    pub bpm: AtomicF32,
    pub latency: AtomicUsize,

    pub in_level: AtomicF32,
    pub out_level: [AtomicF32; 2],
    pub gate_gr: AtomicF32,
    pub deess_gr: AtomicF32,
    pub punch_gr: AtomicF32,
    pub level_gr: AtomicF32,
    pub limit_gr: AtomicF32,
    pub space_level: AtomicF32,

    pub pitch_midi: AtomicF32,
    pub pitch_target: AtomicF32,
    pub pitch_voiced: AtomicBool,
    pub pitch_correction: AtomicF32,
    pub key_root: AtomicI32,
    pub key_minor: AtomicBool,
    pub key_valid: AtomicBool,
    pub key_confidence: AtomicF32,

    /// Set by the editor to start listening; cleared by the audio thread when it starts.
    pub learn_request: AtomicBool,
    /// Set by the editor to stop listening without a result.
    pub learn_cancel: AtomicBool,
    pub learn_state: AtomicU32,
    /// 0..1 of the voiced audio Match needs.
    pub learn_progress: AtomicF32,
    pub learn: LearnResult,
}

impl Default for Shared {
    fn default() -> Self {
        Self {
            sample_rate: AtomicF32::new(48_000.0),
            bpm: AtomicF32::new(120.0),
            latency: AtomicUsize::new(0),
            in_level: AtomicF32::new(0.0),
            out_level: [AtomicF32::new(0.0), AtomicF32::new(0.0)],
            gate_gr: AtomicF32::new(0.0),
            deess_gr: AtomicF32::new(0.0),
            punch_gr: AtomicF32::new(0.0),
            level_gr: AtomicF32::new(0.0),
            limit_gr: AtomicF32::new(0.0),
            space_level: AtomicF32::new(0.0),
            pitch_midi: AtomicF32::new(0.0),
            pitch_target: AtomicF32::new(0.0),
            pitch_voiced: AtomicBool::new(false),
            pitch_correction: AtomicF32::new(0.0),
            key_root: AtomicI32::new(0),
            key_minor: AtomicBool::new(false),
            key_valid: AtomicBool::new(false),
            key_confidence: AtomicF32::new(0.0),
            learn_request: AtomicBool::new(false),
            learn_cancel: AtomicBool::new(false),
            learn_state: AtomicU32::new(LEARN_IDLE),
            learn_progress: AtomicF32::new(0.0),
            learn: LearnResult {
                level_db: AtomicF32::new(-90.0),
                bands_db: std::array::from_fn(|_| AtomicF32::new(-90.0)),
                sibilance_db: AtomicF32::new(-90.0),
                median_midi: AtomicF32::new(0.0),
                key_root: AtomicI32::new(0),
                key_minor: AtomicBool::new(false),
                key_valid: AtomicBool::new(false),
                key_confidence: AtomicF32::new(0.0),
            },
        }
    }
}

impl Shared {
    /// Raises a meter at once and lets it fall slowly, so the editor never misses a peak.
    pub fn meter(slot: &AtomicF32, value: f32) {
        let old = slot.load(Ordering::Relaxed);
        slot.store(if value > old { value } else { old * 0.92 + value * 0.08 }, Ordering::Relaxed);
    }
}
