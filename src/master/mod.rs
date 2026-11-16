//! Persona Master: the mastering chain that ships beside the vocal chain, as a second plugin
//! in the same binary. Parametric and tonal-balance EQ, a glue compressor, four bands of
//! compression, harmonics and stereo width, a soft clipper and a true-peak maximizer, with
//! loudness metering to BS.1770 and an Auto Master that sets it all up from the mix it hears.

pub mod auto;
pub mod editor;
pub mod engine;
pub mod params;
pub mod presets;
pub mod shared;
pub mod spectrum;

use nih_plug::prelude::*;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use engine::{MasterChain, MasterSettings};
use params::MasterParams;
use shared::MasterShared;

/// "60 Hz", "1.8 kHz".
pub fn hz_label(hz: f32) -> String {
    if hz >= 1000.0 {
        let k = hz / 1000.0;
        if (k - k.round()).abs() < 0.05 {
            format!("{k:.0} kHz")
        } else {
            format!("{k:.1} kHz")
        }
    } else {
        format!("{hz:.0} Hz")
    }
}

/// Longest stretch processed with one snapshot of the (smoothed) parameters.
const MAX_BLOCK: usize = 64;

pub struct PersonaMaster {
    params: Arc<MasterParams>,
    shared: Arc<MasterShared>,
    chain: Option<MasterChain>,
    /// A looping test mix that replaces the input in the standalone build (PERSONA_DEMO=1),
    /// so the window can be seen working without a DAW.
    demo: Option<((Vec<f32>, Vec<f32>), usize)>,
}

impl Default for PersonaMaster {
    fn default() -> Self {
        Self { params: Arc::new(MasterParams::default()), shared: Arc::new(MasterShared::default()), chain: None, demo: None }
    }
}

impl Plugin for PersonaMaster {
    const NAME: &'static str = "Persona Master";
    const VENDOR: &'static str = "GPTmadeit";
    const URL: &'static str = env!("CARGO_PKG_REPOSITORY");
    const EMAIL: &'static str = "";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[AudioIOLayout {
        main_input_channels: NonZeroU32::new(2),
        main_output_channels: NonZeroU32::new(2),
        ..AudioIOLayout::const_default()
    }];

    const SAMPLE_ACCURATE_AUTOMATION: bool = false;

    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    fn editor(&mut self, _async_executor: AsyncExecutor<Self>) -> Option<Box<dyn Editor>> {
        editor::create(self.params.clone(), self.shared.clone())
    }

    fn initialize(&mut self, _layout: &AudioIOLayout, config: &BufferConfig, context: &mut impl InitContext<Self>) -> bool {
        let fs = config.sample_rate;
        self.shared.sample_rate.store(fs, Ordering::Relaxed);
        let chain = MasterChain::new(fs);
        // The latency never changes: every stage keeps its delay whether it is working or not.
        context.set_latency_samples(chain.latency() as u32);
        self.shared.latency.store(chain.latency(), Ordering::Relaxed);
        self.chain = Some(chain);
        #[cfg(feature = "standalone")]
        if std::env::var("PERSONA_DEMO").is_ok() {
            self.demo = Some((crate::testsignal::mix(fs, 8.0), 0));
        }
        crate::update::cleanup_old_binaries();
        true
    }

    fn reset(&mut self) {
        if let Some(c) = self.chain.as_mut() {
            c.reset();
        }
    }

    fn process(&mut self, buffer: &mut Buffer, _aux: &mut AuxiliaryBuffers, _context: &mut impl ProcessContext<Self>) -> ProcessStatus {
        let Some(chain) = self.chain.as_mut() else {
            return ProcessStatus::Normal;
        };
        let n = buffer.samples();
        let out = buffer.as_slice();
        let (left, rest) = out.split_at_mut(1);
        let left = &mut left[0][..];
        let right = &mut rest[0][..];
        if let Some(((dl, dr), at)) = self.demo.as_mut() {
            for i in 0..n {
                left[i] = dl[*at];
                right[i] = dr[*at];
                *at = (*at + 1) % dl.len();
            }
        }
        let mut pos = 0;
        while pos < n {
            let end = (pos + MAX_BLOCK).min(n);
            let s = MasterSettings::from_params(&self.params, (end - pos) as u32);
            chain.process(&mut left[pos..end], &mut right[pos..end], &s, Some(&self.shared));
            pos = end;
        }
        ProcessStatus::Normal
    }
}

impl ClapPlugin for PersonaMaster {
    const CLAP_ID: &'static str = "com.gptmadeit.persona-master";
    const CLAP_DESCRIPTION: Option<&'static str> =
        Some("Mastering chain: EQ, tonal balance, glue and multiband compression, exciter, imager and a true-peak maximizer, set up by Auto Master");
    const CLAP_MANUAL_URL: Option<&'static str> = Some(Self::URL);
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] = &[
        ClapFeature::AudioEffect,
        ClapFeature::Stereo,
        ClapFeature::Mastering,
        ClapFeature::Limiter,
        ClapFeature::Equalizer,
        ClapFeature::Compressor,
    ];
}

impl Vst3Plugin for PersonaMaster {
    const VST3_CLASS_ID: [u8; 16] = *b"PersonaMasterGPT";
    const VST3_SUBCATEGORIES: &'static [Vst3SubCategory] =
        &[Vst3SubCategory::Fx, Vst3SubCategory::Mastering, Vst3SubCategory::Dynamics, Vst3SubCategory::Eq];
}
