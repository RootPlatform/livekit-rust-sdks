#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::disallowed_macros,
    clippy::arithmetic_side_effects
)]

use rootapp_voice_dsp::aligner::{
    ring_samples, AlignerCounters, DenoiseFrameAligner, FORCED_DRAIN_FRAMES, PRE_ROLL_FRAMES,
    SURPLUS_DRAIN_FRAMES,
};
use std::collections::VecDeque;

const FRAME: usize = 480;

fn aligner(max_backlog: usize) -> DenoiseFrameAligner {
    DenoiseFrameAligner::new(FRAME, max_backlog, 0)
}

/// A denoiser stand-in that delays its input by `delay` samples and releases output in `chunk`
/// sized pieces, the way a look-ahead model does.
struct DelayLine {
    pending: VecDeque<f32>,
    held: usize,
    chunk: usize,
}

impl DelayLine {
    fn new(delay: usize, chunk: usize) -> Self {
        Self { pending: VecDeque::new(), held: delay, chunk }
    }

    fn process(&mut self, input: &[f32]) -> Vec<f32> {
        self.pending.extend(input.iter().copied());
        let mut ready = self.pending.len().saturating_sub(self.held);
        ready -= ready % self.chunk;
        if ready == 0 {
            return Vec::new();
        }
        self.held = 0;
        self.pending.drain(..ready).collect()
    }
}

fn ramp(start: usize) -> Vec<f32> {
    (start..start + FRAME).map(|i| i as f32).collect()
}

fn run(
    aligner: &mut DenoiseFrameAligner,
    mut pipeline: impl FnMut(&[f32]) -> Vec<f32>,
    frames: usize,
) -> Vec<f32> {
    let mut output = Vec::new();
    let mut frame = [0.0f32; FRAME];
    for f in 0..frames {
        let input = ramp(1 + f * FRAME);
        aligner.push(&pipeline(&input));
        assert!(aligner.pop(&mut frame));
        output.extend_from_slice(&frame);
    }
    output
}

fn leading_silence(output: &[f32]) -> usize {
    output.iter().take_while(|&&v| v == 0.0).count()
}

fn assert_continuous_after_silence(output: &[f32]) -> usize {
    let silence = leading_silence(output);
    for (i, &v) in output[silence..].iter().enumerate() {
        assert_eq!(v, (i + 1) as f32, "sample {i} after {silence} of silence");
    }
    silence
}

/// Never repeated or reordered; any sample skipped is accounted for as drained.
fn assert_every_sample_once_in_order(output: &[f32], aligner: &DenoiseFrameAligner) {
    let audio: Vec<f32> = output.iter().copied().filter(|&v| v != 0.0).collect();
    let mut skipped: i64 = if audio.is_empty() { 0 } else { audio[0] as i64 - 1 };
    for w in audio.windows(2) {
        assert!(w[1] > w[0]);
        skipped += (w[1] - w[0]) as i64 - 1;
    }
    assert_eq!((aligner.drained_samples() + aligner.trimmed_samples()) as i64, skipped);
    assert_eq!(aligner.denoised_samples(), audio.len() as u64);
    assert_eq!(output.len() as u64, aligner.output_samples());
}

#[test]
fn frame_sized_output_passes_straight_through() {
    let mut a = aligner(9600);
    let output = run(&mut a, |input| input.to_vec(), 20);
    assert_eq!(assert_continuous_after_silence(&output), 0);
    assert_eq!(a.inserted_samples(), 0);
    assert_eq!(a.buffered_samples(), 0);
}

#[test]
fn look_ahead_delay_is_primed_with_silence_and_its_catch_up_burst_is_drained() {
    let mut line = DelayLine::new(700, 1);
    let mut a = aligner(9600);
    let output = run(&mut a, |input| line.process(input), 50);

    let silence = leading_silence(&output);
    assert_eq!(output.len(), 50 * FRAME);
    assert_eq!(silence, 960);
    assert_eq!(a.priming_samples(), 960);
    assert_eq!(a.drained_samples(), 960);
    assert_eq!(output[silence], 961.0);
    assert_every_sample_once_in_order(&output, &a);
    assert_eq!(a.underruns(), 0);
    assert_eq!(a.trimmed_samples(), 0);
    assert_eq!(a.buffered_samples(), 0);
}

#[test]
fn chunked_output_settles_after_its_first_underruns() {
    let mut line = DelayLine::new(0, 1024);
    let mut a = aligner(9600);
    let mut output = Vec::new();
    let mut frame = [0.0f32; FRAME];
    let mut underruns_at_halfway = 0;
    for f in 0..400 {
        a.push(&line.process(&ramp(1 + f * FRAME)));
        a.pop(&mut frame);
        output.extend_from_slice(&frame);
        if f == 199 {
            underruns_at_halfway = a.underruns();
        }
    }
    assert_every_sample_once_in_order(&output, &a);
    assert_eq!(a.underruns(), underruns_at_halfway);
    assert_eq!(a.trimmed_samples(), 0);
}

#[test]
fn reset_starts_a_new_pre_roll() {
    let mut a = aligner(9600);
    let mut frame = [0.0f32; FRAME];
    for _ in 0..50 {
        a.push(&ramp(1));
        a.pop(&mut frame);
    }
    a.reset();
    a.push(&[0.0; FRAME * 3]);
    a.pop(&mut frame);
    assert_eq!(a.buffered_samples(), 0);
    assert_eq!(a.drained_samples(), (FRAME * 2) as u64);
}

/// The real SDK's pattern (480 ×5, 960, then 480): the extra pre-roll block is drained instead of
/// becoming a permanent 10 ms of latency.
#[test]
fn the_sdks_extra_pre_roll_block_is_drained_not_kept_as_latency() {
    let mut a = aligner(9600);
    let mut call = 0;
    let output = run(
        &mut a,
        |input| {
            call += 1;
            if call == 6 {
                let mut burst = vec![0.0; FRAME];
                burst.extend_from_slice(input);
                burst
            } else {
                input.to_vec()
            }
        },
        40,
    );
    assert_eq!(a.buffered_samples(), 0);
    assert_eq!(a.drained_samples(), FRAME as u64);
    assert_eq!(a.underruns(), 0);
    assert_eq!(a.trimmed_samples(), 0);
    assert_eq!(a.inserted_samples(), 0);
    assert_eq!(assert_continuous_after_silence(&output), 0);
}

#[test]
fn a_burst_after_the_pre_roll_is_kept_until_it_has_persisted_for_a_second() {
    let mut a = aligner(9600);
    let mut frame = [0.0f32; FRAME];
    let quiet = [0.001f32; FRAME];
    for _ in 0..PRE_ROLL_FRAMES + 5 {
        a.push(&quiet);
        a.pop(&mut frame);
    }
    a.push(&quiet);
    a.push(&quiet);
    a.pop(&mut frame);
    assert_eq!(a.buffered_samples(), FRAME);

    for _ in 0..SURPLUS_DRAIN_FRAMES - 2 {
        a.push(&quiet);
        a.pop(&mut frame);
    }
    assert_eq!(a.buffered_samples(), FRAME);
    assert_eq!(a.drained_samples(), 0);

    a.push(&quiet);
    a.pop(&mut frame);
    assert_eq!(a.buffered_samples(), 0);
    assert_eq!(a.drained_samples(), FRAME as u64);
    assert_eq!(a.underruns(), 0);
}

#[test]
fn a_loud_backlog_waits_for_a_quiet_frame_then_is_forced_out() {
    let mut a = aligner(9600);
    let mut frame = [0.0f32; FRAME];
    let loud = [0.3f32; FRAME];
    for _ in 0..PRE_ROLL_FRAMES {
        a.push(&loud);
        a.pop(&mut frame);
    }
    a.push(&loud);
    a.push(&loud);
    a.pop(&mut frame);

    let mut pops = 1;
    while a.drained_samples() == 0 && pops < FORCED_DRAIN_FRAMES * 2 {
        a.push(&loud);
        a.pop(&mut frame);
        pops += 1;
    }
    assert_eq!(pops, FORCED_DRAIN_FRAMES);
    assert_eq!(a.drained_samples(), FRAME as u64);
    assert_eq!(a.buffered_samples(), 0);
}

#[test]
fn an_underruns_catch_up_is_drained_instead_of_adding_latency_for_the_rest_of_the_call() {
    let mut a = aligner(9600);
    let mut frame = [0.0f32; FRAME];
    let quiet = [0.001f32; FRAME];
    for _ in 0..50 {
        a.push(&quiet);
        a.pop(&mut frame);
    }
    a.pop(&mut frame);
    assert_eq!(a.underruns(), 1);
    a.push(&quiet);
    a.push(&quiet);
    for _ in 0..SURPLUS_DRAIN_FRAMES + 1 {
        a.pop(&mut frame);
        a.push(&quiet);
    }
    a.pop(&mut frame);
    assert_eq!(a.buffered_samples(), 0);
    assert_eq!(a.drained_samples(), FRAME as u64);
}

#[test]
fn underrun_after_priming_pads_silence_and_keeps_every_sample() {
    let mut a = aligner(9600);
    let mut frame = [0.0f32; FRAME];
    a.push(&ramp(1));
    a.pop(&mut frame);
    assert_eq!(frame[0], 1.0);

    a.push(&ramp(FRAME + 1)[..100]);
    a.pop(&mut frame);
    assert_eq!(frame[0], (FRAME + 1) as f32);
    assert_eq!(frame[99], (FRAME + 100) as f32);
    assert!(frame[100..].iter().all(|&v| v == 0.0));
    assert_eq!(a.underruns(), 1);
    assert_eq!(a.padded_samples(), (FRAME - 100) as u64);

    a.push(&ramp(FRAME + 101));
    a.pop(&mut frame);
    assert_eq!(frame[0], (FRAME + 101) as f32);
    assert_eq!(a.trimmed_samples(), 0);
}

#[test]
fn backlog_beyond_the_limit_trims_the_oldest_samples() {
    let mut a = aligner(2400);
    a.push(&[0.0; 3000]);
    assert_eq!(a.buffered_samples(), FRAME);
    assert_eq!(a.trimmed_samples(), (3000 - FRAME) as u64);
}

#[test]
fn a_push_larger_than_the_ring_keeps_the_newest_frame() {
    let mut a = aligner(2400);
    let input: Vec<f32> = (1..=20_000).map(|i| i as f32).collect();
    a.push(&input);
    assert_eq!(a.buffered_samples(), FRAME);
    let mut frame = [0.0f32; FRAME];
    a.pop(&mut frame);
    assert_eq!(frame[0], (20_000 - FRAME + 1) as f32);
    assert_eq!(frame[FRAME - 1], 20_000.0);
    assert_eq!(a.trimmed_samples(), (20_000 - FRAME) as u64);
}

#[test]
fn reset_drops_buffered_audio_and_primes_again() {
    let mut a = aligner(9600);
    let mut frame = [0.0f32; FRAME];
    a.push(&[0.0; FRAME * 2]);
    a.pop(&mut frame);
    assert!(a.is_primed());

    a.reset();
    assert!(!a.is_primed());
    assert_eq!(a.buffered_samples(), 0);
    a.push(&[0.0; 100]);
    frame.fill(1.0);
    a.pop(&mut frame);
    assert!(frame.iter().all(|&v| v == 0.0));
    assert_eq!(a.priming_samples(), FRAME as u64);
}

#[test]
fn pop_requires_exactly_one_frame() {
    let mut a = aligner(9600);
    let mut short = [1.0f32; FRAME - 1];
    assert!(!a.pop(&mut short));
    assert!(short.iter().all(|&v| v == 0.0));
    assert_eq!(a.output_samples(), 0);
}

#[test]
fn wrap_around_keeps_order_over_many_frames() {
    let mut line = DelayLine::new(333, 7);
    let mut a = aligner(4800);
    let output = run(&mut a, |input| line.process(input), 2000);
    assert_every_sample_once_in_order(&output, &a);
    assert_eq!(a.trimmed_samples(), 0);
}

#[test]
fn configure_resizes_within_the_allocation_and_refuses_more() {
    let mut a = DenoiseFrameAligner::new(480, 9600, ring_samples(480, 9600));
    assert!(a.configure(160, 3200));
    assert_eq!((a.frame_samples(), a.max_backlog_samples()), (160, 3200));
    let mut frame = [0.0f32; 160];
    a.push(&[0.5; 160]);
    a.pop(&mut frame);
    assert!(frame.iter().all(|&v| v == 0.5));
    assert!(!a.configure(480, 20_000), "does not fit");
    assert_eq!(a.frame_samples(), 160);
    assert!(a.configure(480, 9600));
    assert!(!a.is_primed());
}

#[test]
fn latency_counts_since_the_reset_and_over_the_lifetime() {
    let mut line = DelayLine::new(700, 1);
    let mut a = aligner(9600);
    run(&mut a, |input| line.process(input), 50);
    assert_eq!(a.lifetime().latency_ms(48000), 0.0, "960 primed, 960 drained");
    a.reset();
    let mut frame = [0.0f32; FRAME];
    a.pop(&mut frame);
    a.pop(&mut frame);
    assert_eq!(a.since_reset(), AlignerCounters { priming_samples: 960, ..Default::default() });
    assert_eq!(a.since_reset().latency_ms(48000), 20.0);
    assert_eq!(a.lifetime().priming_samples, 1920);
    assert_eq!(a.lifetime().latency_ms(48000), 20.0);
    assert_eq!(a.lifetime().latency_ms(16000), 60.0);
}
