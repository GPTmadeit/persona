use nih_plug::prelude::*;
use std::sync::atomic::Ordering;
use std::sync::Arc;

pub mod chain;
pub mod dsp;
pub mod editor;
pub mod params;
pub mod profile;
pub mod shared;
pub mod styles;
#[doc(hidden)]
pub mod testsignal;
pub mod update;

use chain::{Chain, Settings};
use params::PersonaParams;
use shared::Shared;

/// Longest stretch processed with one snapshot of the (smoothed) parameters.
const MAX_BLOCK: usize = 64;

pub struct Persona {
    params: Arc<PersonaParams>,
    shared: Arc<Shared>,
    chain: Option<Chain>,
    mono_in: bool,
    latency: u32,
}

impl Default for Persona {
    fn default() -> Self {
        Self {
            params: Arc::new(PersonaParams::default()),
            shared: Arc::new(Shared::default()),
            chain: None,
            mono_in: false,
            latency: 0,
        }
    }
}

impl Plugin for Persona {
    const NAME: &'static str = "Persona";
    const VENDOR: &'static str = "GPTmadeit";
    const URL: &'static str = env!("CARGO_PKG_REPOSITORY");
    const EMAIL: &'static str = "";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[
        AudioIOLayout {
            main_input_channels: NonZeroU32::new(2),
            main_output_channels: NonZeroU32::new(2),
            ..AudioIOLayout::const_default()
        },
        AudioIOLayout {
            main_input_channels: NonZeroU32::new(1),
            main_output_channels: NonZeroU32::new(2),
            ..AudioIOLayout::const_default()
        },
    ];

    const SAMPLE_ACCURATE_AUTOMATION: bool = false;

    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    fn editor(&mut self, _async_executor: AsyncExecutor<Self>) -> Option<Box<dyn Editor>> {
        editor::create(self.params.clone(), self.shared.clone())
    }

    fn initialize(&mut self, layout: &AudioIOLayout, config: &BufferConfig, context: &mut impl InitContext<Self>) -> bool {
        let fs = config.sample_rate;
        self.shared.sample_rate.store(fs, Ordering::Relaxed);
        self.mono_in = layout.main_input_channels.map(|c| c.get()) == Some(1);
        let mut chain = Chain::new(fs, (config.max_buffer_size as usize).max(MAX_BLOCK));
        let s = Settings::from_params(&self.params, 120.0, 0);
        chain.prepare_range(&s);
        self.latency = chain.latency(&s) as u32;
        context.set_latency_samples(self.latency);
        self.shared.latency.store(self.latency as usize, Ordering::Relaxed);
        self.chain = Some(chain);
        update::cleanup_old_binaries();
        true
    }

    fn reset(&mut self) {
        if let Some(c) = self.chain.as_mut() {
            c.reset();
        }
    }

    fn process(&mut self, buffer: &mut Buffer, _aux: &mut AuxiliaryBuffers, context: &mut impl ProcessContext<Self>) -> ProcessStatus {
        let Some(chain) = self.chain.as_mut() else {
            return ProcessStatus::Normal;
        };
        let bpm = context.transport().tempo.unwrap_or(120.0) as f32;
        self.shared.bpm.store(bpm, Ordering::Relaxed);

        let n = buffer.samples();
        let out = buffer.as_slice();
        let (left, rest) = out.split_at_mut(1);
        let left = &mut left[0][..];
        let right = &mut rest[0][..];
        if self.mono_in {
            // The host only filled the first channel.
            right.copy_from_slice(left);
        }

        let mut pos = 0;
        while pos < n {
            let end = (pos + MAX_BLOCK).min(n);
            let s = Settings::from_params(&self.params, bpm, (end - pos) as u32);
            if pos == 0 {
                chain.prepare_range(&s);
                let latency = chain.latency(&s) as u32;
                if latency != self.latency {
                    self.latency = latency;
                    context.set_latency_samples(latency);
                    self.shared.latency.store(latency as usize, Ordering::Relaxed);
                }
            }
            chain.process(&mut left[pos..end], &mut right[pos..end], &s, Some(&self.shared));
            pos = end;
        }
        ProcessStatus::Normal
    }
}

impl ClapPlugin for Persona {
    const CLAP_ID: &'static str = "com.gptmadeit.persona";
    const CLAP_DESCRIPTION: Option<&'static str> =
        Some("Vocal chain in the style of your favorite rappers: tune, EQ, compression, saturation and space in one plugin");
    const CLAP_MANUAL_URL: Option<&'static str> = Some(Self::URL);
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] = &[
        ClapFeature::AudioEffect,
        ClapFeature::Stereo,
        ClapFeature::Mono,
        ClapFeature::PitchCorrection,
        ClapFeature::Compressor,
        ClapFeature::Equalizer,
        ClapFeature::Reverb,
        ClapFeature::Delay,
    ];
}

impl Vst3Plugin for Persona {
    const VST3_CLASS_ID: [u8; 16] = *b"PersonaVocalsGPT";
    const VST3_SUBCATEGORIES: &'static [Vst3SubCategory] =
        &[Vst3SubCategory::Fx, Vst3SubCategory::PitchShift, Vst3SubCategory::Dynamics, Vst3SubCategory::Reverb];
}

nih_export_clap!(Persona);
nih_export_vst3!(Persona);
