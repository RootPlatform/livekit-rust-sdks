#![allow(unsafe_code)]
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::disallowed_macros,
    clippy::arithmetic_side_effects,
    clippy::manual_is_multiple_of
)]

mod support;

use rootapp_voice_dsp::voice_gain::{VoiceGain, VoiceGainOptions};
use rootapp_voice_dsp::{
    DenoiserOutput, DspChain, DspCommands, DspOutputs, DspSettings, PushToTalk, MAX_FRAME,
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use support::Rng;

/// Counts allocations and deallocations made by the current thread while counting is on, so
/// libtest's other threads cannot add to them.
struct Counting;

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static EVENTS: Cell<u64> = const { Cell::new(0) };
}

fn note() {
    let _ = COUNTING.try_with(|counting| {
        if counting.get() {
            let _ = EVENTS.try_with(|events| events.set(events.get() + 1));
        }
    });
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note();
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note();
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        note();
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note();
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn counted(f: impl FnOnce()) -> u64 {
    EVENTS.with(|events| events.set(0));
    COUNTING.with(|counting| counting.set(true));
    f();
    COUNTING.with(|counting| counting.set(false));
    EVENTS.with(|events| events.get())
}

#[test]
fn the_counter_sees_allocations() {
    let events = counted(|| {
        let v = std::hint::black_box(vec![1u8; 64]);
        drop(v);
    });
    assert_eq!(events, 2);
}

const RATES: [u32; 8] = [8000, 11025, 16000, 22050, 24000, 32000, 44100, 48000];

/// 10 000 frames: every rate, rate changes, every command, the three denoiser kinds (empty,
/// short, burst and oversized outputs, restarts), unsupported shapes and changing settings.
#[test]
fn the_chain_never_allocates_or_frees_after_new() {
    let mut chain = DspChain::new(48_000);
    let mut out = DspOutputs::default();
    let mut rng = Rng::new(11);
    let input: Vec<f32> =
        (0..48_000).map(|i| 8000.0 * ((i as f32) * 0.03).sin() + 300.0 * rng.symmetric()).collect();
    let denoised: Vec<f32> =
        (0..8 * MAX_FRAME + 500).map(|i| 4000.0 * ((i as f32) * 0.02).sin()).collect();
    let mut frame = vec![0.0f32; 2 * MAX_FRAME];

    let events = counted(|| {
        let mut position = 0usize;
        for f in 0..10_000usize {
            let rate = RATES[(f / 50) % RATES.len()];
            let n = (rate / 100) as usize;
            let len = match f % 23 {
                7 => n - 1,
                13 => n + 1,
                19 => 0,
                _ => n,
            };
            let settings = DspSettings {
                sample_rate: rate,
                mic_muted: f % 97 > 80,
                push_to_talk: PushToTalk::from_wire(((f / 31) % 4) as u32),
                input_volume: [1.0, 0.5, 2.0, 0.0][(f / 70) % 4],
                auto_gain: f % 400 < 300,
                gate_threshold: [0.025, 0.0, 0.3][(f / 90) % 3],
                auto_sensitivity: f % 300 < 200,
                denoiser_wanted: f % 1000 < 600,
            };
            let commands = DspCommands {
                reset: f % 997 == 3,
                flush: f % 211 == 5,
                gain_reset: f % 157 == 9,
                gain_reset_db: (f % 19) as f32,
            };
            let frame = &mut frame[..len];
            if position + len > input.len() {
                position = 0;
            }
            frame.copy_from_slice(&input[position..position + len]);
            position += len;
            let output = match f % 11 {
                0 | 1 => DenoiserOutput::NotInUse,
                2 => DenoiserOutput::Missed,
                3 => DenoiserOutput::Ran { samples: &denoised[..0], restart: false },
                4 => DenoiserOutput::Ran { samples: &denoised[..2 * n], restart: f % 5 == 0 },
                5 => DenoiserOutput::Ran { samples: &denoised[..8 * n + 500], restart: false },
                6 => DenoiserOutput::Ran { samples: &denoised[..n / 2], restart: false },
                _ => DenoiserOutput::Ran { samples: &denoised[..n], restart: false },
            };
            chain.pre_denoise(frame, &settings, &commands, &mut out);
            chain.post_denoise(frame, output, &settings, &mut out);
            if f % 41 == 0 {
                chain.process(frame, &settings, &commands, &mut out);
            }
        }
    });
    assert_eq!(events, 0, "allocations and frees after new");
}

/// Managed VoiceGainTests.ProcessingAllocatesNothing.
#[test]
fn voice_gain_processing_allocates_nothing() {
    let mut gain = VoiceGain::new(0.0, VoiceGainOptions::default());
    let mut frame: Vec<f32> = (0..480).map(|i| 0.01 * (i as f32 * 0.05).sin()).collect();
    for _ in 0..200 {
        gain.process(&mut frame);
    }
    let events = counted(|| {
        for _ in 0..2000 {
            gain.observe_input(&frame);
            gain.process(&mut frame);
        }
        gain.reset(6.0);
    });
    assert_eq!(events, 0);
}
