// Copyright 2026 LiveKit, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! The port check (estimator-spec.md section 8.3): golden traces the
//! research's reference estimator recorded inside AEC3 ("EMR2": u32 records,
//! u32 record bytes, u32 onset guard blocks, then per call u8 far end active,
//! u8 dominant nearend, u8 pad[2], f32 render[64], y[64], e[64], S2[65],
//! Y2[65], E2[65], R2[65]), replayed through this estimator with the trace's
//! far-end flags.
//!
//! Steps 2-8 keep the reference's arithmetic, and so does the executor with
//! the portable kernel: R2 is bit-exact there (on the platform the traces
//! were recorded on). The SIMD kernels order the dot products' sums
//! differently, so with the kernel the CPU picks R2 is held to the spec's
//! tolerance: at least 99.9% of bins with |dR2| <= 1e-3 (min(Y2, E2) + 1).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use rootapp_echo_model::{Estimator, Hook, Kernel, Model};
use sha2::{Digest, Sha256};

/// The native test's two-tone beeps scene (48 kHz, stream delay 0), f32
/// weights: synthesized, recorded by the research probe.
const NATIVE_BEEPS: &[u8] = include_bytes!("data/native-beeps.emr");
const NATIVE_BEEPS_SHA256: &str =
    "fdc9cc2b341715ae4b4f8976f694293af3a6e8b556179aaf1e4b6195ce036a6d";
const DT_LAPTOP_SHA256: &str = "51cd911ffba3e5b5f7c51902aadcfae786fa4583ecde7ec6be88e379c29e271f";

const RECORD_BYTES: usize = 4 + 4 * (64 * 3 + 65 * 4);

/// The traces were recorded on x86-64 Windows: elsewhere the C library's
/// sin and cos (the windows and the render DFT) and rustfft's kernels round
/// differently, so only the tolerance holds there.
const RECORDED_HERE: bool = cfg!(all(target_arch = "x86_64", target_os = "windows"));

#[derive(Debug, Default)]
struct Replay {
    calls: usize,
    bins: usize,
    exact: usize,
    within: usize,
    worst_relative: f64,
    active_calls: usize,
    onsets: usize,
    nearend_calls: usize,
    diverged_calls: usize,
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn f32s(bytes: &[u8]) -> Vec<f32> {
    bytes.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

fn replay(trace: &[u8], kernel: Kernel) -> Replay {
    assert_eq!(&trace[..4], b"EMR2");
    let u32_at =
        |o: usize| u32::from_le_bytes([trace[o], trace[o + 1], trace[o + 2], trace[o + 3]]);
    let (calls, record, guard) = (u32_at(4) as usize, u32_at(8) as usize, u32_at(12));
    assert_eq!(record, RECORD_BYTES);
    assert_eq!(trace.len(), 16 + calls * record);
    let model = Arc::new(Model::embedded().unwrap().with_kernel(kernel).unwrap());
    let mut estimator = Estimator::new(model);
    let mut stats = Replay { calls, ..Replay::default() };
    let mut was_active = false;
    for call in trace[16..].chunks_exact(record) {
        let v = f32s(&call[4..]);
        let (render, y, e) = (&v[0..64], &v[64..128], &v[128..192]);
        let (s2, y2, e2, want) = (&v[192..257], &v[257..322], &v[322..387], &v[387..452]);
        let (active, nearend) = (call[0] != 0, call[1] != 0);
        let hook = Hook {
            render,
            y,
            e,
            s2,
            y2,
            e2,
            dominant_nearend: nearend,
            far_end_active: active,
            onset_guard_blocks: guard,
        };
        let mut r2 = [0.0f32; 65];
        estimator.estimate(&hook, &mut r2);
        stats.active_calls += usize::from(active);
        stats.onsets += usize::from(active && !was_active);
        was_active = active;
        stats.nearend_calls += usize::from(nearend);
        let (sy, se): (f32, f32) = (y2.iter().sum(), e2.iter().sum());
        if active && !nearend && se > 1.5 * sy && sy > 64.0 * 64.0 * 900.0 {
            stats.diverged_calls += 1;
        }
        for k in 0..65 {
            stats.bins += 1;
            stats.exact += usize::from(r2[k].to_bits() == want[k].to_bits());
            let rel = f64::from((r2[k] - want[k]).abs()) / (f64::from(y2[k].min(e2[k])) + 1.0);
            stats.worst_relative = stats.worst_relative.max(rel);
            stats.within += usize::from(rel <= 1e-3);
        }
    }
    stats
}

fn check(name: &str, trace: &[u8]) {
    for kernel in [Kernel::Scalar, Kernel::detect()] {
        let s = replay(trace, kernel);
        let share = |n: usize| 100.0 * n as f64 / s.bins as f64;
        eprintln!(
            "{name} ({kernel:?}): {} calls, far end active {}, onsets {}, dominant nearend {}, diverged {}; R2 bit-exact {:.3}% of bins, within 1e-3 {:.4}%, worst {:.2e}",
            s.calls,
            s.active_calls,
            s.onsets,
            s.nearend_calls,
            s.diverged_calls,
            share(s.exact),
            share(s.within),
            s.worst_relative
        );
        assert!(s.active_calls > 0 && s.onsets > 1 && s.nearend_calls > 0, "{name}: {s:?}");
        if kernel == Kernel::Scalar && RECORDED_HERE {
            assert_eq!(s.exact, s.bins, "{name}: the portable kernel keeps the reference's order");
        }
        assert!(share(s.within) >= 99.9, "{name} ({kernel:?}): {s:?}");
    }
}

#[test]
fn the_native_beeps_trace_replays_within_the_spec_tolerance() {
    assert_eq!(sha256(NATIVE_BEEPS), NATIVE_BEEPS_SHA256);
    check("native-beeps", NATIVE_BEEPS);
}

/// The 16 kHz double-talk laptop scene (6-11 s, stream delay 40 ms): derived
/// from the AEC Challenge's synthetic set, so it is not in the repository.
/// Needs ROOTAPP_ECHO_GOLDEN_DIR (the research's `echo/out/golden`):
/// `cargo test -- --ignored` with it set.
#[test]
#[ignore = "needs ROOTAPP_ECHO_GOLDEN_DIR (dt-laptop-6s-11s.emr)"]
fn the_double_talk_trace_replays_within_the_spec_tolerance() {
    let dir = std::env::var_os("ROOTAPP_ECHO_GOLDEN_DIR")
        .expect("ROOTAPP_ECHO_GOLDEN_DIR is not set: this test cannot run without it");
    let trace = std::fs::read(std::path::Path::new(&dir).join("dt-laptop-6s-11s.emr")).unwrap();
    assert_eq!(sha256(&trace), DT_LAPTOP_SHA256);
    check("dt-laptop-6s-11s", &trace);
    let s = replay(&trace, Kernel::detect());
    assert!(s.diverged_calls > 0, "{s:?}");
}
