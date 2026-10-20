#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::disallowed_macros,
    clippy::arithmetic_side_effects
)]

//! Golden vectors from the managed reference (desktop `VoiceDspGoldenVectors`, spec §4): every
//! `*.rdv` in tests/golden (and in $ROOTAPP_DSP_GOLDEN_EXTRA) through one `DspChain::new(48000)`.
//! Sent samples and RMS values must agree within about 4 ulp (|a − e| ≤ 2^-22·max(|e|, 2^-15));
//! ROOTAPP_DSP_GOLDEN_LOOSE=1 uses the looser bounds meant for other platforms' math libraries.
//! ROOTAPP_DSP_GOLDEN_REPORT=1 prints the largest deviation of every field per vector.

use rootapp_voice_dsp::{
    DenoiserOutput, DspChain, DspCommands, DspOutputs, DspSettings, PushToTalk, SpeechDetection,
};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 8] = b"RDSPVEC1";
const FRAME_TAG: u32 = 0x4D41_5246;
const SUMSQ_TO_UNIT: f64 = 1.0 / 1_073_741_824.0;
const VALUE_NAMES: [&str; 7] = [
    "sent_rms",
    "denoised_rms",
    "gate_in_rms",
    "gate_in_block_peak_db",
    "threshold",
    "envelope",
    "voice_gain_db",
];
const FLAG_NAMES: [&str; 8] = [
    "sent",
    "gate_speaking",
    "gate_open",
    "gate_input_above",
    "held",
    "voice_gain_settled",
    "denoiser_stalled",
    "edge",
];

struct Record {
    settings: DspSettings,
    commands: DspCommands,
    input: Vec<f32>,
    kind: u8,
    restart: bool,
    denoised: Vec<f32>,
    flags: u8,
    sent: Option<Vec<f32>>,
    values: [f32; 7],
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let slice = self
            .bytes
            .get(self.at..self.at + n)
            .ok_or_else(|| format!("truncated at byte {} (wanted {n})", self.at))?;
        self.at += n;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn i16(&mut self) -> Result<i16, String> {
        Ok(i16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn f32(&mut self) -> Result<f32, String> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
}

fn injected(kind: u8) -> f32 {
    match kind {
        1 => f32::NAN,
        2 => f32::INFINITY,
        3 => f32::NEG_INFINITY,
        4 => 1e-40,
        _ => 1e30,
    }
}

type Header = Vec<(String, String)>;

fn parse(bytes: &[u8]) -> Result<(Header, Vec<Record>), String> {
    let mut r = Reader { bytes, at: 0 };
    if r.take(8)? != MAGIC {
        return Err("bad magic".into());
    }
    let header_len = r.u32()? as usize;
    let header_text = std::str::from_utf8(r.take(header_len)?).map_err(|e| e.to_string())?;
    let header = header_text
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let mut records = Vec::new();
    while r.at < bytes.len() {
        let index = records.len();
        let tag = r.u32()?;
        if tag != FRAME_TAG {
            return Err(format!("record {index}: tag {tag:#x}"));
        }
        let rate = r.u32()?;
        let n = r.u32()? as usize;
        let b = r.take(8)?.to_vec();
        let input_volume = r.f32()?;
        let gate_threshold = r.f32()?;
        let gain_reset_db = r.f32()?;
        let speech_probability = if b[0] & 4 != 0 { r.f32()? } else { -1.0 };
        let mut input = Vec::with_capacity(n);
        for _ in 0..n {
            input.push(f32::from(r.i16()?));
        }
        let injections = r.u16()?;
        for _ in 0..injections {
            let at = r.u16()? as usize;
            let kind = r.u8()?;
            *input.get_mut(at).ok_or_else(|| format!("record {index}: injection at {at}"))? =
                injected(kind);
        }
        let kind = r.u8()?;
        let restart = r.u8()? != 0;
        let as_i16 = r.u8()? == 1;
        let m = r.u32()? as usize;
        let mut denoised = Vec::with_capacity(m);
        for _ in 0..m {
            denoised.push(if as_i16 { f32::from(r.i16()?) } else { r.f32()? });
        }
        let flags = r.u8()?;
        let sent = if flags & 1 != 0 {
            let mut sent = Vec::with_capacity(n);
            for _ in 0..n {
                sent.push(r.f32()?);
            }
            Some(sent)
        } else {
            None
        };
        let mut values = [0.0f32; 7];
        for v in values.iter_mut() {
            *v = r.f32()?;
        }
        records.push(Record {
            settings: DspSettings {
                sample_rate: rate,
                mic_muted: b[0] & 1 != 0,
                capture_silent: b[0] & 2 != 0,
                push_to_talk: PushToTalk::from_wire(u32::from(b[1])),
                input_volume,
                auto_gain: b[2] != 0,
                gate_threshold,
                auto_sensitivity: b[3] != 0,
                denoiser_wanted: b[4] != 0,
                speech_probability,
                ..DspSettings::default()
            },
            commands: DspCommands {
                reset: b[5] != 0,
                flush: b[6] != 0,
                gain_reset: b[7] != 0,
                gain_reset_db,
            },
            input,
            kind,
            restart,
            denoised,
            flags,
            sent,
            values,
        });
    }
    Ok((header, records))
}

#[derive(Clone, Copy)]
struct Tolerance {
    loose: bool,
}

impl Tolerance {
    /// Sent samples (±1 units).
    fn sample(self, actual: f32, expected: f32) -> bool {
        let (a, e) = (f64::from(actual), f64::from(expected));
        if self.loose {
            (a - e).abs() <= 1e-6 + 1e-5 * e.abs()
        } else {
            (a - e).abs() <= 2f64.powi(-22) * e.abs().max(2f64.powi(-15))
        }
    }

    fn rms(self, actual: f32, expected: f32) -> bool {
        let (a, e) = (f64::from(actual), f64::from(expected));
        if self.loose {
            (a - e).abs() <= 1e-9 + 1e-5 * e.abs()
        } else {
            (a - e).abs() <= 2f64.powi(-22) * e.abs().max(2f64.powi(-15))
        }
    }

    fn value(self, which: usize, actual: f32, expected: f32) -> bool {
        if actual == expected {
            return true;
        }
        let (a, e) = (f64::from(actual), f64::from(expected));
        match which {
            0..=2 => self.rms(actual, expected),
            3 => (a - e).abs() <= 1e-3,
            4 => (a - e).abs() <= 1e-5 * e.abs(),
            5 => (a - e).abs() <= 1e-6,
            _ => (a - e).abs() <= 1e-4,
        }
    }
}

/// The chain's outputs in the vectors' terms.
fn actual_values(out: &DspOutputs, frame: &[f32]) -> [f32; 7] {
    let n = frame.len().max(1) as f64;
    let mut sent = 0.0f64;
    for &x in frame {
        let v = f64::from(x / 32768.0);
        sent += v * v;
    }
    [
        if frame.is_empty() { 0.0 } else { (sent / n).sqrt() as f32 },
        ((out.denoised_sumsq * SUMSQ_TO_UNIT) / n).sqrt() as f32,
        out.gate_in_rms,
        out.gate_in_block_peak_db,
        out.threshold,
        out.envelope,
        out.voice_gain_db,
    ]
}

fn actual_flags(out: &DspOutputs) -> u8 {
    (if out.gate_speaking { 0x02 } else { 0 })
        | (if out.gate_open { 0x04 } else { 0 })
        | (if out.gate_input_above { 0x08 } else { 0 })
        | (if out.held { 0x10 } else { 0 })
        | (if out.voice_gain_settled { 0x20 } else { 0 })
        | (if out.denoiser_stalled { 0x40 } else { 0 })
}

/// Largest deviation per field, for the report.
#[derive(Default)]
struct Deviation {
    sent_ulps: f64,
    sent_exact: u64,
    sent_compared: u64,
    values: [f64; 7],
}

fn ulps(actual: f32, expected: f32) -> f64 {
    (f64::from(actual) - f64::from(expected)).abs()
        / (2f64.powi(-24) * f64::from(expected).abs().max(2f64.powi(-15)))
}

struct Outcome {
    frames: usize,
    failures: Vec<String>,
    failed_frames: usize,
    deviation: Deviation,
    transitions_moved: Vec<String>,
}

fn run_vector(
    records: &[Record],
    tolerance: Tolerance,
    speech: impl Fn(usize, &mut DspSettings),
) -> Outcome {
    let mut chain = DspChain::new(48_000);
    let mut out = DspOutputs::default();
    let mut failures = Vec::new();
    let mut failed_frames = 0usize;
    let mut deviation = Deviation::default();
    let mut flag_history: Vec<(u8, u8)> = Vec::with_capacity(records.len());
    let mut transitions_moved = Vec::new();

    for (index, record) in records.iter().enumerate() {
        let mut frame = record.input.clone();
        let mut settings = record.settings;
        speech(index, &mut settings);
        let pre = chain.pre_denoise(&mut frame, &settings, &record.commands, &mut out);
        let denoised = match record.kind {
            0 => DenoiserOutput::NotInUse,
            1 => DenoiserOutput::Missed,
            _ => DenoiserOutput::Ran { samples: &record.denoised, restart: record.restart },
        };
        chain.post_denoise(&mut frame, denoised, &settings, &mut out);

        let unsupported = !pre.supported;
        let edge = record.flags & 0x80 != 0;
        let rate = record.settings.sample_rate;
        let fade = ((u64::from(rate) * 5 / 1000).max(1)) as usize;
        let values = actual_values(&out, &frame);
        let flags = actual_flags(&out);
        let mut problems = Vec::new();

        let flag_mask = 0x7Eu8;
        let expected_flags = record.flags & flag_mask;
        flag_history.push((expected_flags & 0x06, flags & 0x06));
        if flags & flag_mask != expected_flags {
            for (bit, name) in FLAG_NAMES.iter().enumerate().take(7).skip(1) {
                let mask = 1u8 << bit;
                if flag_mask & mask != 0 && (flags & mask) != (record.flags & mask) {
                    problems.push(format!(
                        "{name} {} vs expected {}",
                        flags & mask != 0,
                        record.flags & mask != 0
                    ));
                }
            }
        }

        for (which, (&a, &e)) in values.iter().zip(record.values.iter()).enumerate() {
            let skip = which == 0 && edge;
            if skip {
                continue;
            }
            let both_neg_inf = a == f32::NEG_INFINITY && e == f32::NEG_INFINITY;
            if !both_neg_inf {
                let d = if which <= 2 { ulps(a, e) } else { (f64::from(a) - f64::from(e)).abs() };
                if d.is_finite() {
                    deviation.values[which] = deviation.values[which].max(d);
                } else {
                    deviation.values[which] = f64::INFINITY;
                }
            }
            if !both_neg_inf && !tolerance.value(which, a, e) {
                problems.push(format!("{} {a:e} vs expected {e:e}", VALUE_NAMES[which]));
            }
        }

        if let Some(expected) = &record.sent {
            if expected.len() != frame.len() {
                problems.push(format!(
                    "sent length {} vs expected {}",
                    frame.len(),
                    expected.len()
                ));
            } else {
                let mut bad = 0usize;
                let mut first_bad = None;
                for (i, (&x, &e)) in frame.iter().zip(expected.iter()).enumerate() {
                    if edge && i < fade {
                        continue;
                    }
                    let a = x / 32768.0;
                    deviation.sent_compared += 1;
                    if a == e {
                        deviation.sent_exact += 1;
                    } else {
                        deviation.sent_ulps = deviation.sent_ulps.max(ulps(a, e));
                    }
                    if !tolerance.sample(a, e) {
                        bad += 1;
                        first_bad.get_or_insert((i, a, e));
                    }
                }
                if let Some((i, a, e)) = first_bad {
                    problems.push(format!(
                        "{bad} sent samples differ, first at {i}: {a:e} vs expected {e:e}"
                    ));
                }
            }
        }

        if !problems.is_empty() {
            failed_frames += 1;
            if failures.len() < 5 {
                let mut text = String::new();
                let _ = write!(
                    text,
                    "frame {index} (rate {rate}, n {}, kind {}, restart {}, ptt {:?}, cmds {:?}{}{}):",
                    record.input.len(),
                    record.kind,
                    record.restart,
                    record.settings.push_to_talk,
                    record.commands,
                    if unsupported { ", unsupported" } else { "" },
                    if edge { ", transmit edge" } else { "" },
                );
                for p in &problems {
                    let _ = write!(text, "\n      {p}");
                }
                let _ =
                    write!(text, "\n      all: flags {flags:#04x} vs {:#04x}", record.flags & 0x7E);
                for (which, name) in VALUE_NAMES.iter().enumerate() {
                    let _ =
                        write!(text, "; {name} {:e} vs {:e}", values[which], record.values[which]);
                }
                failures.push(text);
            }
        }
    }

    for (index, window) in flag_history.windows(2).enumerate() {
        let (expected_before, actual_before) = window[0];
        let (expected_after, actual_after) = window[1];
        if expected_before != expected_after && actual_before == actual_after {
            if let Some(moved) =
                flag_history[index + 1..].iter().position(|&(_, actual)| actual == expected_after)
            {
                if moved > 0 && transitions_moved.len() < 5 {
                    transitions_moved
                        .push(format!("frame {}: transition moved by {moved} frames", index + 1));
                }
            }
        }
    }

    Outcome { frames: records.len(), failures, failed_frames, deviation, transitions_moved }
}

fn vector_files() -> Vec<PathBuf> {
    let mut dirs = vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("golden")];
    if let Ok(extra) = std::env::var("ROOTAPP_DSP_GOLDEN_EXTRA") {
        dirs.extend(
            extra.split(';').filter(|s| !s.trim().is_empty()).map(|s| PathBuf::from(s.trim())),
        );
    }
    let mut files = Vec::new();
    for dir in dirs {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "rdv") {
                    files.push(path);
                }
            }
        }
    }
    files.sort();
    files
}

#[test]
fn golden_vectors_match_the_managed_reference() {
    let files = vector_files();
    assert!(!files.is_empty(), "no *.rdv in tests/golden");
    let tolerance =
        Tolerance { loose: std::env::var("ROOTAPP_DSP_GOLDEN_LOOSE").is_ok_and(|v| v == "1") };
    let report = std::env::var("ROOTAPP_DSP_GOLDEN_REPORT").is_ok_and(|v| v == "1");
    let mut failed = Vec::new();
    for path in &files {
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        let bytes = std::fs::read(path).unwrap();
        let (header, records) = match parse(&bytes) {
            Ok(parsed) => parsed,
            Err(e) => {
                failed.push(format!("{name}: {e}"));
                continue;
            }
        };
        let declared = header.iter().find(|(k, _)| k == "frames").map(|(_, v)| v.as_str());
        if declared != Some(records.len().to_string().as_str()) {
            failed.push(format!(
                "{name}: header says {declared:?} frames, file has {}",
                records.len()
            ));
        }
        let outcome = run_vector(&records, tolerance, |_, _| {});
        let d = &outcome.deviation;
        if report {
            println!(
                "{name}: {} frames, {} failing; sent {}/{} exact, max {:.2} ulp; rms max ulp sent {:.2} denoised {:.2} gate_in {:.2}; \
                 peak_db {:.2e} threshold {:.2e} envelope {:.2e} gain_db {:.2e}",
                outcome.frames,
                outcome.failed_frames,
                d.sent_exact,
                d.sent_compared,
                d.sent_ulps,
                d.values[0],
                d.values[1],
                d.values[2],
                d.values[3],
                d.values[4],
                d.values[5],
                d.values[6]
            );
        }
        if outcome.failed_frames > 0 {
            let mut text =
                format!("{name}: {} of {} frames differ", outcome.failed_frames, outcome.frames);
            for t in &outcome.transitions_moved {
                let _ = write!(text, "\n    {t}");
            }
            for f in &outcome.failures {
                let _ = write!(text, "\n    {f}");
            }
            failed.push(text);
        }
    }
    assert!(
        failed.is_empty(),
        "{} of {} vectors failed:\n{}",
        failed.len(),
        files.len(),
        failed.join("\n")
    );
}

type Tweak = Box<dyn Fn(usize, &mut DspSettings)>;

/// Every frame's sent audio, values and flags through one chain, with `tweak` applied to each
/// record's settings.
fn run_chain(
    records: &[Record],
    tweak: impl Fn(usize, &mut DspSettings),
) -> Vec<(Vec<f32>, [f32; 7], u8)> {
    let mut chain = DspChain::new(48_000);
    let mut out = DspOutputs::default();
    let mut frames = Vec::with_capacity(records.len());
    for (index, record) in records.iter().enumerate() {
        let mut frame = record.input.clone();
        let mut settings = record.settings;
        tweak(index, &mut settings);
        chain.pre_denoise(&mut frame, &settings, &record.commands, &mut out);
        let denoised = match record.kind {
            0 => DenoiserOutput::NotInUse,
            1 => DenoiserOutput::Missed,
            _ => DenoiserOutput::Ran { samples: &record.denoised, restart: record.restart },
        };
        chain.post_denoise(&mut frame, denoised, &settings, &mut out);
        let values = actual_values(&out, &frame);
        frames.push((frame, values, actual_flags(&out)));
    }
    frames
}

/// The split the capture processor uses for a host's gain (post_denoise_align, then post_gain)
/// with the chain's own voice gain is post_denoise: every vector's sent frames and every output,
/// bit for bit.
#[test]
fn golden_vectors_align_and_post_gain_equal_post_denoise() {
    let mut failed = Vec::new();
    for path in vector_files() {
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        let (_, records) = parse(&std::fs::read(&path).unwrap()).unwrap();
        let mut whole = DspChain::new(48_000);
        let mut split = DspChain::new(48_000);
        let mut whole_out = DspOutputs::default();
        let mut split_out = DspOutputs::default();
        for (index, record) in records.iter().enumerate() {
            let denoised = match record.kind {
                0 => DenoiserOutput::NotInUse,
                1 => DenoiserOutput::Missed,
                _ => DenoiserOutput::Ran { samples: &record.denoised, restart: record.restart },
            };
            let mut a = record.input.clone();
            whole.pre_denoise(&mut a, &record.settings, &record.commands, &mut whole_out);
            whole.post_denoise(&mut a, denoised, &record.settings, &mut whole_out);
            let mut b = record.input.clone();
            split.pre_denoise(&mut b, &record.settings, &record.commands, &mut split_out);
            let aligned =
                split.post_denoise_align(&mut b, denoised, &record.settings, &mut split_out);
            split.post_gain(
                &mut b,
                aligned.pre_gain_rms,
                f32::NAN,
                &record.settings,
                &mut split_out,
            );
            let same_frame = a.iter().map(|x| x.to_bits()).eq(b.iter().map(|x| x.to_bits()));
            if !same_frame || format!("{whole_out:?}") != format!("{split_out:?}") {
                failed.push(format!("{name}: frame {index} differs"));
                break;
            }
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

/// The speech detector's paths leave the audio as it is when they change nothing: Ring mode (it
/// qualifies the ring only) sends what the managed reference sent with each vector's own
/// probabilities, and bit for bit what Off sends with any probability; Gate mode with a
/// probability that never stops being speech (it never closes) and no look-ahead sends what Off
/// sends. The voice gain takes the probability in every mode, so each pair compares the same
/// probabilities. Gate mode's look-ahead is checked by golden_vectors_gate_with_constant_speech_
/// is_off_delayed_by_the_look_ahead.
#[test]
fn golden_vectors_hold_where_speech_detection_changes_no_audio() {
    let tolerance =
        Tolerance { loose: std::env::var("ROOTAPP_DSP_GOLDEN_LOOSE").is_ok_and(|v| v == "1") };
    const ANY: [f32; 8] = [0.9, 0.1, -1.0, f32::NAN, 0.6, 0.0, 0.35, 2.0];
    let mut failed = Vec::new();
    for path in vector_files() {
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        let (_, records) = parse(&std::fs::read(&path).unwrap()).unwrap();
        let ring =
            run_vector(&records, tolerance, |_, s| s.speech_detection = SpeechDetection::Ring);
        if ring.failed_frames > 0 {
            failed.push(format!(
                "{name} (ring, its own probabilities): {} of {} frames differ\n    {}",
                ring.failed_frames,
                ring.frames,
                ring.failures.join("\n    ")
            ));
        }
        let any = |f: usize| ANY[(f / 3) % ANY.len()];
        let pairs: [(&str, Tweak, Tweak); 2] = [
            (
                "ring vs off, any probability",
                Box::new(move |f, s| {
                    s.speech_detection = SpeechDetection::Ring;
                    s.speech_probability = any(f);
                }),
                Box::new(move |f, s| s.speech_probability = any(f)),
            ),
            (
                "gate vs off, constant speech, no look-ahead",
                Box::new(|_, s| {
                    s.speech_detection = SpeechDetection::Gate;
                    s.speech_probability = 1.0;
                    s.speech_lookahead_ms = 0.0;
                }),
                Box::new(|_, s| s.speech_probability = 1.0),
            ),
        ];
        for (label, mode, off) in pairs {
            let a = run_chain(&records, mode);
            let b = run_chain(&records, off);
            if let Some(f) = (0..a.len()).find(|&f| a[f] != b[f]) {
                failed.push(format!("{name} ({label}): frame {f} differs"));
            }
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

/// Gate mode with constant speech and the default look-ahead (4 frames) decides exactly as Off on
/// every frame: the gate measures the frame it is handed, so the threshold, the envelope, the
/// voice gain, the levels before the gate and the gate's flags are Off's, frame for frame. What it
/// sends is the audio 4 frames older under that decision. Wherever Off's gain was the same over
/// both frames (full on, or shut) and nothing in between changed the stream or the transmit and
/// volume settings, frame f sends exactly what Off sent at frame f - 4, as does its gate input
/// and its startup hold flag: the look-ahead is a pure delay of the audio.
#[test]
fn golden_vectors_gate_with_constant_speech_is_off_delayed_by_the_look_ahead() {
    const DELAY: usize = 4;
    const FULL: f32 = 0.2 + 0.025;
    let mut failed = Vec::new();
    let mut compared = 0usize;
    for path in vector_files() {
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        let (_, records) = parse(&std::fs::read(&path).unwrap()).unwrap();
        let gate = run_chain(&records, |_, s| {
            s.speech_detection = SpeechDetection::Gate;
            s.speech_probability = 1.0;
        });
        let off = run_chain(&records, |_, s| s.speech_probability = 1.0);
        let decided = |v: &[f32; 7]| [v[1], v[2], v[4], v[5], v[6]];
        let decision_flags = 0x02 | 0x04 | 0x08 | 0x20 | 0x40;
        for f in 0..records.len() {
            let ((_, gv, gflags), (_, ov, oflags)) = (&gate[f], &off[f]);
            if decided(gv) != decided(ov) || gflags & decision_flags != oflags & decision_flags {
                failed.push(format!("{name}: frame {f} decides differently"));
                break;
            }
        }
        let supported = |r: &Record| {
            r.input.len() == (r.settings.sample_rate / 100) as usize && !r.input.is_empty()
        };
        let steady = |f: usize| {
            f >= DELAY + 3
                && (f - DELAY - 3..=f).all(|k| {
                    let (a, b) = (&records[k], &records[f]);
                    supported(a)
                        && a.settings.sample_rate == b.settings.sample_rate
                        && a.settings.push_to_talk == PushToTalk::VoiceActivity
                        && a.settings.mic_muted == b.settings.mic_muted
                        && a.settings.input_volume == b.settings.input_volume
                        && !a.commands.reset
                        && !a.commands.flush
                        && a.flags & 0x80 == 0
                })
        };
        let envelope = |f: usize| off[f].1[5];
        for f in DELAY + 3..records.len() {
            if !steady(f) {
                continue;
            }
            let span = [f - DELAY - 1, f - DELAY, f - 1, f];
            let full = span.iter().all(|&k| envelope(k) >= FULL);
            let shut = span.iter().all(|&k| envelope(k) == 0.0);
            if !full && !shut {
                continue;
            }
            compared += 1;
            let (g, o) = (&gate[f], &off[f - DELAY]);
            let same_audio = g.0.iter().map(|x| x.to_bits()).eq(o.0.iter().map(|x| x.to_bits()));
            if !same_audio || g.1[3].to_bits() != o.1[3].to_bits() || g.2 & 0x10 != o.2 & 0x10 {
                failed.push(format!(
                    "{name}: frame {f} is not Off's frame {} ({})",
                    f - DELAY,
                    if full { "full" } else { "shut" }
                ));
                break;
            }
        }
    }
    assert!(compared > 10_000, "only {compared} frames were steady enough to compare");
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}
