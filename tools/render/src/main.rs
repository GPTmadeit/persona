//! Minimal offline CLAP host used for testing the real plugin binary.
//!
//! `cargo run -p render --release -- <path/to/Persona.clap> [out_dir]`
//!
//! Loads every style through the plugin's state extension (the same path a DAW uses when it
//! reopens a project), runs a synthetic rap vocal through it, writes a WAV per style and checks
//! the output. It also checks the reported latency against the audio, that the tuner lands on
//! the note, that the delay follows the host tempo and that the state round-trips.

use clap_sys::audio_buffer::clap_audio_buffer;
use clap_sys::entry::clap_plugin_entry;
use clap_sys::events::*;
use clap_sys::ext::latency::{clap_plugin_latency, CLAP_EXT_LATENCY};
use clap_sys::ext::state::{clap_plugin_state, CLAP_EXT_STATE};
use clap_sys::factory::plugin_factory::{clap_plugin_factory, CLAP_PLUGIN_FACTORY_ID};
use clap_sys::fixedpoint::CLAP_BEATTIME_FACTOR;
use clap_sys::host::clap_host;
use clap_sys::plugin::clap_plugin;
use clap_sys::process::clap_process;
use clap_sys::stream::{clap_istream, clap_ostream};
use clap_sys::version::CLAP_VERSION;
use serde_json::Value;
use std::f32::consts::TAU;
use std::ffi::{c_char, c_void, CString};
use std::path::{Path, PathBuf};

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;

unsafe extern "C" fn host_get_extension(_: *const clap_host, _: *const c_char) -> *const c_void {
    std::ptr::null()
}
unsafe extern "C" fn host_noop(_: *const clap_host) {}

unsafe extern "C" fn ev_size(_: *const clap_input_events) -> u32 {
    0
}
unsafe extern "C" fn ev_get(_: *const clap_input_events, _: u32) -> *const clap_event_header {
    std::ptr::null()
}
unsafe extern "C" fn out_push(_: *const clap_output_events, _: *const clap_event_header) -> bool {
    true
}

unsafe extern "C" fn ostream_write(s: *const clap_ostream, buf: *const c_void, size: u64) -> i64 {
    let v = &mut *((*s).ctx as *mut Vec<u8>);
    v.extend_from_slice(std::slice::from_raw_parts(buf as *const u8, size as usize));
    size as i64
}

struct ReadCursor {
    data: Vec<u8>,
    pos: usize,
}

unsafe extern "C" fn istream_read(s: *const clap_istream, buf: *mut c_void, size: u64) -> i64 {
    let c = &mut *((*s).ctx as *mut ReadCursor);
    let n = (c.data.len() - c.pos).min(size as usize);
    std::ptr::copy_nonoverlapping(c.data.as_ptr().add(c.pos), buf as *mut u8, n);
    c.pos += n;
    n as i64
}

struct Plugin {
    _lib: libloading::Library,
    _host: Box<clap_host>,
    plugin: *const clap_plugin,
    state: *const clap_plugin_state,
    latency: *const clap_plugin_latency,
}

impl Plugin {
    unsafe fn load(path: &Path) -> Self {
        // On Linux a .clap is a plain shared object.
        let lib = libloading::Library::new(path).expect("load plugin");
        let entry: libloading::Symbol<*const clap_plugin_entry> = lib.get(b"clap_entry\0").expect("clap_entry");
        let entry = &**entry;
        let cpath = CString::new(path.to_string_lossy().as_bytes()).unwrap();
        assert!((entry.init.unwrap())(cpath.as_ptr()));
        let factory = (entry.get_factory.unwrap())(CLAP_PLUGIN_FACTORY_ID.as_ptr()) as *const clap_plugin_factory;
        assert!(!factory.is_null());
        assert_eq!((*factory).get_plugin_count.unwrap()(factory), 1);
        let desc = (*factory).get_plugin_descriptor.unwrap()(factory, 0);
        let host = Box::new(clap_host {
            clap_version: CLAP_VERSION,
            host_data: std::ptr::null_mut(),
            name: c"persona-render".as_ptr(),
            vendor: c"test".as_ptr(),
            url: c"".as_ptr(),
            version: c"1".as_ptr(),
            get_extension: Some(host_get_extension),
            request_restart: Some(host_noop),
            request_process: Some(host_noop),
            request_callback: Some(host_noop),
        });
        let plugin = (*factory).create_plugin.unwrap()(factory, &*host, (*desc).id);
        assert!(!plugin.is_null());
        assert!((*plugin).init.unwrap()(plugin));
        let state = (*plugin).get_extension.unwrap()(plugin, CLAP_EXT_STATE.as_ptr()) as *const clap_plugin_state;
        assert!(!state.is_null());
        let latency = (*plugin).get_extension.unwrap()(plugin, CLAP_EXT_LATENCY.as_ptr()) as *const clap_plugin_latency;
        assert!(!latency.is_null());
        assert!((*plugin).activate.unwrap()(plugin, SR, 1, BLOCK as u32));
        Plugin { _lib: lib, _host: host, plugin, state, latency }
    }

    unsafe fn save(&self) -> Value {
        let mut buf: Vec<u8> = Vec::new();
        let os = clap_ostream { ctx: &mut buf as *mut _ as *mut c_void, write: Some(ostream_write) };
        assert!((*self.state).save.unwrap()(self.plugin, &os));
        // nih-plug writes a little-endian u64 length, then the JSON.
        let len = u64::from_le_bytes(buf[..8].try_into().unwrap()) as usize;
        serde_json::from_slice(&buf[8..8 + len]).expect("state is json")
    }

    unsafe fn load_state(&self, v: &Value) {
        let json = serde_json::to_vec(v).unwrap();
        let mut data = (json.len() as u64).to_le_bytes().to_vec();
        data.extend_from_slice(&json);
        let mut cur = ReadCursor { data, pos: 0 };
        let is = clap_istream { ctx: &mut cur as *mut _ as *mut c_void, read: Some(istream_read) };
        assert!((*self.state).load.unwrap()(self.plugin, &is), "state load failed");
    }

    unsafe fn latency(&self) -> u32 {
        (*self.latency).get.unwrap()(self.plugin)
    }

    /// Processes `input` (mono, sent to both channels) with the transport running at `bpm`.
    unsafe fn render(&self, bpm: f64, input: &[f32]) -> (Vec<f32>, Vec<f32>) {
        let p = self.plugin;
        (*p).reset.unwrap()(p);
        assert!((*p).start_processing.unwrap()(p));
        let total = input.len();
        let mut out_l = vec![0.0f32; total];
        let mut out_r = vec![0.0f32; total];
        let (mut il, mut ir) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
        let (mut ol, mut or) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
        let spb = 60.0 / bpm * SR;
        let mut pos = 0usize;
        while pos < total {
            let n = BLOCK.min(total - pos);
            il[..n].copy_from_slice(&input[pos..pos + n]);
            ir[..n].copy_from_slice(&input[pos..pos + n]);
            let in_ev = clap_input_events { ctx: std::ptr::null_mut(), size: Some(ev_size), get: Some(ev_get) };
            let out_ev = clap_output_events { ctx: std::ptr::null_mut(), try_push: Some(out_push) };
            let beat_pos = pos as f64 / spb;
            let transport = clap_event_transport {
                header: clap_event_header {
                    size: std::mem::size_of::<clap_event_transport>() as u32,
                    time: 0,
                    space_id: CLAP_CORE_EVENT_SPACE_ID,
                    type_: CLAP_EVENT_TRANSPORT,
                    flags: 0,
                },
                flags: CLAP_TRANSPORT_HAS_TEMPO | CLAP_TRANSPORT_HAS_BEATS_TIMELINE | CLAP_TRANSPORT_IS_PLAYING | CLAP_TRANSPORT_HAS_TIME_SIGNATURE,
                song_pos_beats: (beat_pos * CLAP_BEATTIME_FACTOR as f64) as i64,
                song_pos_seconds: 0,
                tempo: bpm,
                tempo_inc: 0.0,
                loop_start_beats: 0,
                loop_end_beats: 0,
                loop_start_seconds: 0,
                loop_end_seconds: 0,
                bar_start: ((beat_pos / 4.0).floor() * 4.0 * CLAP_BEATTIME_FACTOR as f64) as i64,
                bar_number: (beat_pos / 4.0) as i32,
                tsig_num: 4,
                tsig_denom: 4,
            };
            let mut in_chans = [il.as_mut_ptr(), ir.as_mut_ptr()];
            let mut out_chans = [ol.as_mut_ptr(), or.as_mut_ptr()];
            let inp = clap_audio_buffer { data32: in_chans.as_mut_ptr(), data64: std::ptr::null_mut(), channel_count: 2, latency: 0, constant_mask: 0 };
            let mut out = clap_audio_buffer { data32: out_chans.as_mut_ptr(), data64: std::ptr::null_mut(), channel_count: 2, latency: 0, constant_mask: 0 };
            let proc_ = clap_process {
                steady_time: pos as i64,
                frames_count: n as u32,
                transport: &transport,
                audio_inputs: &inp,
                audio_outputs: &mut out,
                audio_inputs_count: 1,
                audio_outputs_count: 1,
                in_events: &in_ev,
                out_events: &out_ev,
            };
            let status = (*p).process.unwrap()(p, &proc_);
            assert!(status != 0, "process returned error");
            out_l[pos..pos + n].copy_from_slice(&ol[..n]);
            out_r[pos..pos + n].copy_from_slice(&or[..n]);
            pos += n;
        }
        (*p).stop_processing.unwrap()(p);
        (out_l, out_r)
    }
}

/// Applies `values` (id -> plain value) on top of the default state.
fn patched(default: &Value, values: &[(&str, f32)]) -> Value {
    let mut st = default.clone();
    let params = st["params"].as_object_mut().unwrap();
    for (id, v) in values {
        let slot = params.get_mut(*id).unwrap_or_else(|| panic!("unknown param {id}"));
        let (kind, _) = slot.as_object().unwrap().iter().next().map(|(k, v)| (k.clone(), v.clone())).unwrap();
        *slot = match kind.as_str() {
            "f32" => serde_json::json!({ "f32": v }),
            "i32" => serde_json::json!({ "i32": v.round() as i32 }),
            "bool" => serde_json::json!({ "bool": *v > 0.5 }),
            other => panic!("unsupported param kind {other} for {id}"),
        };
    }
    st
}

fn write_wav(path: &Path, l: &[f32], r: &[f32]) {
    let spec = hound::WavSpec { channels: 2, sample_rate: SR as u32, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    for (a, b) in l.iter().zip(r) {
        w.write_sample(*a).unwrap();
        w.write_sample(*b).unwrap();
    }
    w.finalize().unwrap();
}

fn rms_db(x: &[f32]) -> f32 {
    let ms = x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32;
    10.0 * ms.max(1e-12).log10()
}

fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0.0f32, |m, v| m.max(v.abs()))
}

/// Energy above `hz` relative to the whole signal, dB, through a 2nd-order high-pass.
fn above_db(x: &[f32], hz: f32) -> f32 {
    let w = TAU * hz / SR as f32;
    let (cs, al) = (w.cos(), w.sin() / (2.0 * 0.707));
    let a0 = 1.0 + al;
    let (b0, b1, b2, a1, a2) = ((1.0 + cs) / 2.0 / a0, -(1.0 + cs) / a0, (1.0 + cs) / 2.0 / a0, -2.0 * cs / a0, (1.0 - al) / a0);
    let (mut x1, mut x2, mut y1, mut y2) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    let hp: Vec<f32> = x
        .iter()
        .map(|&v| {
            let y = b0 * v + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2;
            (x2, x1, y2, y1) = (x1, v, y1, y);
            y
        })
        .collect();
    rms_db(&hp) - rms_db(x)
}

/// Stereo width (side over mid, dB), the level in the gaps between phrases relative to the
/// whole take (how much echo and room hangs on), and the share of energy above 5 kHz.
fn character(l: &[f32], r: &[f32]) -> (f32, f32, f32) {
    let mid: Vec<f32> = l.iter().zip(r).map(|(a, b)| 0.5 * (a + b)).collect();
    let side: Vec<f32> = l.iter().zip(r).map(|(a, b)| 0.5 * (a - b)).collect();
    let fs = SR as usize;
    let gaps: Vec<f32> = (0..3).flat_map(|k| mid[(k * 2 * fs + fs * 175 / 100)..(k * 2 * fs + fs * 198 / 100)].to_vec()).collect();
    (rms_db(&side) - rms_db(&mid), rms_db(&gaps) - rms_db(&mid), above_db(&mid, 5000.0))
}

/// Frequency of a steady tone from its zero crossings.
fn tone_hz(x: &[f32]) -> f32 {
    let mut first = None;
    let mut last = 0.0;
    let mut count = 0;
    for i in 1..x.len() {
        if x[i - 1] < 0.0 && x[i] >= 0.0 {
            let frac = x[i - 1] / (x[i - 1] - x[i]);
            let at = (i - 1) as f32 + frac;
            if first.is_none() {
                first = Some(at);
            } else {
                count += 1;
            }
            last = at;
        }
    }
    match first {
        Some(f) if count > 0 => count as f32 / ((last - f) / SR as f32),
        _ => 0.0,
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let plugin_path = PathBuf::from(args.get(1).expect("usage: render <Persona.clap> [out_dir]"));
    let out_dir = PathBuf::from(args.get(2).cloned().unwrap_or_else(|| "target/renders".into()));
    std::fs::create_dir_all(&out_dir).unwrap();
    let mut failures = Vec::new();
    let input = persona::testsignal::vocal(SR as f32, 6.0);
    write_wav(&out_dir.join("_input.wav"), &input, &input);
    let in_db = rms_db(&input);
    println!("test vocal: {in_db:.1} dBFS RMS, peak {:.2}", peak(&input));

    unsafe {
        let pl = Plugin::load(&plugin_path);
        let default = pl.save();

        // ---- every style
        let (w0, t0, a0) = character(&input, &input);
        println!("{:<22} {:>7} {:>8} {:>8} {:>7} {:>7} {:>7} {:>7}", "style", "peak", "rms dB", "gain dB", "lat", "width", "tail", "air");
        println!("{:<22} {:>7} {:>8} {:>8} {:>7} {:>7.1} {:>7.1} {:>7.1}", "(input)", "", "", "", "", w0.max(-99.0), t0, a0);
        let mut gains = Vec::new();
        for style in persona::styles::STYLES {
            let st = patched(&default, style.values);
            pl.load_state(&st);
            let (l, r) = pl.render(120.0, &input);
            let pk = peak(&l).max(peak(&r));
            let db = rms_db(&l).max(rms_db(&r));
            let finite = l.iter().chain(&r).all(|v| v.is_finite());
            let name = style.name.replace([' ', '/', ',', '$', '.'], "_");
            write_wav(&out_dir.join(format!("{name}.wav")), &l, &r);
            let gain = db - in_db;
            gains.push(gain);
            // The limiter's ceiling defaults to -1 dBFS; allow its tiny overshoot.
            let ok = finite && pk <= 0.9 && gain > -12.0 && gain < 18.0;
            let (w, t, a) = character(&l, &r);
            println!(
                "{:<22} {:>7.3} {:>8.1} {:>+8.1} {:>7} {:>7.1} {:>7.1} {:>7.1} {}",
                style.name, pk, db, gain, pl.latency(), w.max(-99.0), t, a, if ok { "ok" } else { "FAIL" }
            );
            if !ok {
                failures.push(format!("{}: finite={finite} peak={pk} gain={gain:.1} dB", style.name));
            }
        }
        let (lo, hi) = gains.iter().fold((f32::MAX, f32::MIN), |(a, b), g| (a.min(*g), b.max(*g)));
        println!("styles change the level by {lo:+.1} to {hi:+.1} dB");

        // ---- dry signal and latency line up: at 0% mix the output is the input, delayed by the
        // latency the plugin reports.
        for tune in [0.0, 1.0] {
            pl.load_state(&patched(&default, &[("mix", 0.0), ("t_on", tune)]));
            let (l, _) = pl.render(120.0, &input);
            let lat = pl.latency() as usize;
            let err = (lat..input.len()).map(|i| (l[i] - input[i - lat]).abs()).fold(0.0f32, f32::max);
            println!("latency with tune {}: {lat} samples, dry error {err:.2e}", if tune > 0.5 { "on" } else { "off" });
            if err > 1e-4 {
                failures.push(format!("dry path doesn't match the reported latency ({lat}) with tune={tune}"));
            }
        }

        // ---- the tuner lands a flat note on the scale: A3 40 cents flat, C major, fast retune.
        let fs = SR as f32;
        let flat = 220.0 * 2f32.powf(-0.4 / 12.0);
        let tone: Vec<f32> = (0..(fs * 3.0) as usize)
            .map(|i| {
                let t = i as f32 / fs;
                (1..6).map(|h| (TAU * flat * h as f32 * t).sin() / h as f32).sum::<f32>() * 0.25
            })
            .collect();
        pl.load_state(&patched(
            &default,
            &[("t_on", 1.0), ("t_key", 0.0), ("t_scale", 1.0), ("t_speed", 0.0), ("t_voice", 1.0), ("fet_on", 0.0), ("opt_on", 0.0), ("ds_on", 0.0)],
        ));
        let (l, _) = pl.render(120.0, &tone);
        let before = tone_hz(&tone[tone.len() - fs as usize..]);
        let after = tone_hz(&l[l.len() - fs as usize..]);
        let cents = 1200.0 * (after / 220.0).log2();
        println!("tune: {before:.1} Hz in, {after:.1} Hz out ({cents:+.1} cents from A3)");
        if cents.abs() > 5.0 {
            failures.push(format!("tuner missed the note: {after:.1} Hz"));
        }

        // ---- the delay follows the host tempo: 1/4 note at 100 BPM is 600 ms.
        for bpm in [100.0, 140.0] {
            let mut click = vec![0.0f32; (fs * 2.0) as usize];
            click[1000] = 0.5;
            pl.load_state(&patched(
                &default,
                &[("dly_on", 1.0), ("dly_div", 5.0), ("dly_fb", 0.0), ("dly_mix", 0.5), ("dly_duck", 0.0), ("dly_pp", 0.0), ("dly_hp", 60.0), ("dly_lp", 16000.0), ("fet_on", 0.0), ("opt_on", 0.0), ("ds_on", 0.0), ("hp_on", 0.0)],
            ));
            let (l, _) = pl.render(bpm, &click);
            let lat = pl.latency() as usize;
            let start = 1000 + lat + 2000;
            let echo = (start..l.len()).max_by(|a, b| l[*a].abs().total_cmp(&l[*b].abs())).unwrap();
            let expected = 1000 + lat + (60.0 / bpm * SR) as usize;
            let off = echo as i64 - expected as i64;
            println!("delay at {bpm} BPM: echo at {echo}, expected {expected} ({off:+} samples)");
            if off.abs() > 24 {
                failures.push(format!("delay off the beat at {bpm} BPM ({off:+} samples)"));
            }
        }

        // ---- state round trip, with the persisted style name and voice profile
        let style = persona::styles::find("Travis Scott").unwrap();
        let mut st = patched(&default, style.values);
        st["fields"]["style"] = Value::String(serde_json::to_string(style.name).unwrap());
        let voice = persona::profile::VoiceProfile { learned: true, sib_excess_db: 2.5, hp_cap_hz: 95.0 };
        st["fields"]["voice"] = Value::String(serde_json::to_string(&voice).unwrap());
        pl.load_state(&st);
        let back = pl.save();
        // Values are f32 inside the plugin, so compare at f32 precision.
        let same = |a: &Value, b: &Value| match (a.get("f32"), b.get("f32")) {
            (Some(x), Some(y)) => x.as_f64().map(|v| v as f32) == y.as_f64().map(|v| v as f32),
            _ => a == b,
        };
        let params = st["params"].as_object().unwrap();
        let mut bad = params.iter().filter(|(k, v)| !same(&back["params"][k.as_str()], v)).map(|(k, _)| k.clone()).collect::<Vec<_>>();
        for f in ["style", "voice"] {
            if back["fields"][f] != st["fields"][f] {
                bad.push(format!("fields.{f}: {} vs {}", back["fields"][f], st["fields"][f]));
            }
        }
        if bad.is_empty() {
            println!("state round-trip ok");
        } else {
            failures.push(format!("state did not round-trip: {}", bad.join(", ")));
        }

        (*pl.plugin).deactivate.unwrap()(pl.plugin);
        (*pl.plugin).destroy.unwrap()(pl.plugin);
    }

    if failures.is_empty() {
        println!("ALL CHECKS PASSED  (wavs in {})", out_dir.display());
    } else {
        for f in &failures {
            eprintln!("FAIL: {f}");
        }
        std::process::exit(1);
    }
}
