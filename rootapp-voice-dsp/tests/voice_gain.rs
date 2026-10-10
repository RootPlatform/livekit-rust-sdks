#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::disallowed_macros,
    clippy::arithmetic_side_effects
)]

mod support;

use rootapp_voice_dsp::voice_gain::{VoiceGain, VoiceGainOptions};
use std::f32::consts::PI;
use support::{in_range, near, Rng};

const FRAME: usize = 480;
const RATE: f32 = 48000.0;

fn new_gain(start_db: f64) -> VoiceGain {
    VoiceGain::new(start_db, VoiceGainOptions::default())
}

fn amplitude(rms_db: f64) -> f32 {
    (10f64.powf(rms_db / 20.0) * 2f64.sqrt()) as f32
}

/// Speech-like: 200 ms "syllables" of a tone at the level, 100 ms pauses at the floor.
fn talk(
    gain: &mut VoiceGain,
    level_db: f64,
    seconds: f64,
    floor_db: f64,
    mut output_db: Option<&mut Vec<f64>>,
) {
    let mut frame = [0.0f32; FRAME];
    let mut rng = Rng::new(3);
    let frames = (seconds * 100.0) as usize;
    for f in 0..frames {
        let voiced = f % 30 < 20;
        let a = if voiced { amplitude(level_db) } else { 0.0 };
        let n = (10f64.powf(floor_db / 20.0) * 3f64.sqrt()) as f32;
        for (i, s) in frame.iter_mut().enumerate() {
            *s = a * (2.0 * PI * 220.0 * (f * FRAME + i) as f32 / RATE).sin() + n * rng.symmetric();
        }
        gain.process(&mut frame);
        if voiced {
            if let Some(levels) = output_db.as_deref_mut() {
                let mean =
                    frame.iter().map(|&s| f64::from(s) * f64::from(s)).sum::<f64>() / FRAME as f64;
                levels.push(10.0 * mean.log10());
            }
        }
    }
}

fn talk_quiet_room(gain: &mut VoiceGain, level_db: f64, seconds: f64) {
    talk(gain, level_db, seconds, -95.0, None);
}

#[test]
fn it_holds_the_starting_gain_until_it_has_heard_half_a_second_of_speech() {
    let mut gain = new_gain(6.0);
    talk_quiet_room(&mut gain, -46.0, 0.6);
    assert!(near(gain.gain_db(), 6.0, 1e-6), "{}", gain.gain_db());
}

#[test]
fn a_quiet_talker_is_brought_up_to_the_target_within_a_few_seconds() {
    let mut gain = new_gain(0.0);
    talk_quiet_room(&mut gain, -38.0, 6.0);
    assert!(in_range(gain.gain_db(), 15.0, 17.5), "{}", gain.gain_db());
    assert!(gain.is_settled());
}

#[test]
fn a_very_quiet_talker_gets_at_most_the_maximum_boost() {
    let mut gain = new_gain(0.0);
    talk_quiet_room(&mut gain, -46.0, 6.0);
    assert!(near(gain.gain_db(), VoiceGainOptions::default().max_gain_db, 1e-3));
}

#[test]
fn a_loud_talker_is_never_turned_down() {
    let mut gain = new_gain(0.0);
    talk_quiet_room(&mut gain, -12.0, 6.0);
    assert!(near(gain.gain_db(), 0.0, 1e-6));
}

#[test]
fn steady_noise_is_not_speech_and_gets_no_gain() {
    let mut gain = new_gain(0.0);
    let mut frame = [0.0f32; FRAME];
    let mut rng = Rng::new(1);
    let a = (10f64.powf(-50.0 / 20.0) * 3f64.sqrt()) as f32;
    for _ in 0..1000 {
        for s in frame.iter_mut() {
            *s = a * rng.symmetric();
        }
        gain.process(&mut frame);
    }
    assert!(near(gain.gain_db(), 0.0, 1e-6));
    assert_eq!(gain.speech_ms(), 0.0);
}

#[test]
fn the_gain_never_lifts_the_noise_floor_above_the_cap() {
    let mut gain = new_gain(0.0);
    talk(&mut gain, -42.0, 8.0, -62.0, None);
    assert!(in_range(gain.floor_db(), -66.0, -58.0), "{}", gain.floor_db());
    let cap = VoiceGainOptions::default().max_output_noise_db - gain.floor_db() + 0.5;
    assert!(
        gain.gain_db() <= cap,
        "gain {} dB over a {} dB floor",
        gain.gain_db(),
        gain.floor_db()
    );
}

fn click(frame: &mut [f32; FRAME], peak: f32) {
    frame.fill(0.0);
    for (i, s) in frame.iter_mut().take(192).enumerate() {
        *s = peak * (-(i as f32) / 48.0).exp() * if i % 2 == 0 { 1.0 } else { -1.0 };
    }
}

#[test]
fn key_clicks_are_never_speech() {
    let mut gain = new_gain(0.0);
    let mut frame = [0.0f32; FRAME];
    for f in 0..1000 {
        if f % 25 == 0 {
            click(&mut frame, 0.3);
        } else {
            frame.fill(0.0);
        }
        gain.process(&mut frame);
    }
    assert_eq!(gain.speech_ms(), 0.0);
    assert!(near(gain.gain_db(), 0.0, 1e-6));
    assert!(
        gain.noise_db() > -60.0,
        "clicks should raise the noise estimate, got {}",
        gain.noise_db()
    );
}

#[test]
fn clicks_in_a_pause_cap_a_quiet_talkers_gain() {
    let mut gain = new_gain(0.0);
    talk_quiet_room(&mut gain, -46.0, 6.0);
    let talking = gain.gain_db();

    let mut frame = [0.0f32; FRAME];
    for f in 0..400 {
        if f % 25 == 0 {
            click(&mut frame, 0.2);
        } else {
            frame.fill(0.0);
        }
        gain.process(&mut frame);
    }

    assert!(in_range(talking, 17.0, VoiceGainOptions::default().max_gain_db), "{talking}");
    assert!(gain.gain_db() < talking - 6.0, "gain {} after typing vs {talking}", gain.gain_db());
    assert!(gain.noise_db() > -65.0, "noise estimate {}", gain.noise_db());
}

#[test]
fn the_limiter_keeps_a_boosted_peak_under_the_ceiling() {
    let mut gain = new_gain(30.0);
    let mut frame: Vec<f32> =
        (0..FRAME).map(|i| 0.8 * (2.0 * PI * 440.0 * i as f32 / RATE).sin()).collect();
    gain.process(&mut frame);
    let peak = frame.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
    assert!(peak <= VoiceGainOptions::default().limiter_ceiling + 1e-4, "{peak}");
}

#[test]
fn starting_from_the_learned_gain_the_first_seconds_match_the_rest() {
    let mut learning = new_gain(0.0);
    talk_quiet_room(&mut learning, -40.0, 8.0);
    assert!(learning.is_settled());

    let mut next_call = new_gain(learning.gain_db());
    let mut levels = Vec::new();
    talk(&mut next_call, -40.0, 8.0, -95.0, Some(&mut levels));
    let first_second = levels.iter().take(66).sum::<f64>() / 66.0;
    let tail = &levels[levels.len() - 200..];
    let last_seconds = tail.iter().sum::<f64>() / tail.len() as f64;
    assert!(in_range(first_second - last_seconds, -1.0, 1.0), "{first_second} vs {last_seconds}");
}

/// The input before the denoiser holds the room, the denoised frame mostly the voice.
fn room_talk(
    gain: &mut VoiceGain,
    voice_db: Option<f64>,
    room_db: f64,
    seconds: f64,
    other_db: Option<f64>,
) {
    let mut input = [0.0f32; FRAME];
    let mut output = [0.0f32; FRAME];
    let mut rng = Rng::new(5);
    let room = (10f64.powf(room_db / 20.0) * 3f64.sqrt()) as f32;
    let residue = (10f64.powf(-85.0 / 20.0) * 3f64.sqrt()) as f32;
    for f in 0..(seconds * 100.0) as usize {
        let voiced = voice_db.is_some() && f % 150 < 100 && f % 30 < 20;
        let other_voiced = other_db.is_some() && (f + 7) % 25 < 15;
        let a = if voiced { amplitude(voice_db.unwrap()) } else { 0.0 };
        let b = if other_voiced { amplitude(other_db.unwrap()) } else { 0.0 };
        for i in 0..FRAME {
            let n = (f * FRAME + i) as f32;
            let voice =
                a * (2.0 * PI * 220.0 * n / RATE).sin() + b * (2.0 * PI * 310.0 * n / RATE).sin();
            input[i] = voice + room * rng.symmetric();
            output[i] = voice + residue * rng.symmetric();
        }
        gain.observe_input(&input);
        gain.process(&mut output);
    }
}

#[test]
fn a_noisy_room_gets_no_boost_even_when_the_denoiser_clears_the_pauses() {
    let mut gain = new_gain(0.0);
    room_talk(&mut gain, Some(-38.0), -40.0, 8.0, None);
    assert!(in_range(gain.room_noise_db(), -43.0, -38.0), "{}", gain.room_noise_db());
    assert!(near(gain.gain_db(), 0.0, 0.05), "{}", gain.gain_db());
}

#[test]
fn a_quiet_room_still_boosts_a_quiet_talker() {
    let mut gain = new_gain(0.0);
    room_talk(&mut gain, Some(-38.0), -75.0, 8.0, None);
    assert!(in_range(gain.room_noise_db(), -78.0, -70.0), "{}", gain.room_noise_db());
    assert!(in_range(gain.gain_db(), 15.0, 17.5), "{}", gain.gain_db());
}

#[test]
fn a_room_in_between_limits_the_boost_to_what_keeps_it_under_the_cap() {
    let mut gain = new_gain(0.0);
    room_talk(&mut gain, Some(-38.0), -58.0, 8.0, None);
    let limit = VoiceGainOptions::default().max_room_noise_db - gain.room_noise_db();
    assert!(in_range(limit, 4.0, 12.0), "{limit}");
    assert!(in_range(gain.gain_db(), limit - 0.5, limit + 0.1), "{} vs {limit}", gain.gain_db());
}

#[test]
fn a_voice_well_under_the_person_counts_as_the_room() {
    let mut gain = new_gain(0.0);
    room_talk(&mut gain, Some(-30.0), -80.0, 5.0, None);
    let speech_level = gain.speech_level_db();
    assert!(in_range(gain.gain_db(), 6.0, 9.0), "{}", gain.gain_db());

    room_talk(&mut gain, None, -80.0, 6.0, Some(-50.0));

    assert!(in_range(gain.speech_level_db(), speech_level - 1.0, speech_level + 1.0));
    assert!(in_range(gain.room_noise_db(), -56.0, -49.0), "{}", gain.room_noise_db());
    let cap = VoiceGainOptions::default().max_room_noise_db - gain.room_noise_db() + 0.1;
    assert!(in_range(gain.gain_db(), 0.0, cap), "{} vs {cap}", gain.gain_db());
}

#[test]
fn a_remembered_gain_falls_in_a_noisy_room_before_anyone_speaks() {
    let mut gain = new_gain(VoiceGainOptions::default().max_gain_db);
    room_talk(&mut gain, None, -45.0, 4.0, None);
    assert_eq!(gain.speech_ms(), 0.0);
    assert!(near(gain.gain_db(), 0.0, 0.05), "{}", gain.gain_db());
}

#[test]
fn a_remembered_gain_is_kept_in_a_quiet_room_until_the_person_speaks() {
    let mut gain = new_gain(12.0);
    room_talk(&mut gain, None, -80.0, 4.0, None);
    assert!(near(gain.gain_db(), 12.0, 1e-6), "{}", gain.gain_db());
}

#[test]
fn reset_starts_over_from_the_given_gain() {
    let mut gain = new_gain(0.0);
    talk_quiet_room(&mut gain, -46.0, 5.0);
    gain.reset(3.0);
    assert!(near(gain.gain_db(), 3.0, 1e-6));
    assert!(!gain.is_settled());
    assert_eq!(gain.speech_ms(), 0.0);
}

#[test]
fn start_gains_are_clamped_and_nan_is_zero() {
    assert_eq!(new_gain(f64::NAN).gain_db(), 0.0);
    assert_eq!(new_gain(-4.0).gain_db(), 0.0);
    assert_eq!(new_gain(f64::INFINITY).gain_db(), 18.0);
    assert!(new_gain(0.0).room_noise_db().is_nan());
    assert_eq!(new_gain(0.0).speech_level_db(), f64::NEG_INFINITY);
    assert_eq!(new_gain(0.0).noise_db(), -90.0);
}

#[test]
fn unity_gain_leaves_the_frame_untouched() {
    let mut gain = new_gain(0.0);
    let input: Vec<f32> = (0..FRAME).map(|i| 0.01 * (i as f32 * 0.05).sin()).collect();
    let mut frame = input.clone();
    gain.process(&mut frame);
    assert_eq!(frame, input);
    gain.process(&mut []);
    gain.observe_input(&[]);
}

/// 100 s of audio has to cost a tiny fraction of real time.
#[test]
fn it_costs_under_one_percent_of_real_time() {
    let mut gain = new_gain(0.0);
    let mut frame: Vec<f32> = (0..FRAME).map(|i| 0.01 * (i as f32 * 0.05).sin()).collect();
    for _ in 0..500 {
        gain.process(&mut frame);
    }
    let clock = std::time::Instant::now();
    for _ in 0..10_000 {
        gain.process(&mut frame);
    }
    let elapsed = clock.elapsed();
    assert!(elapsed < std::time::Duration::from_secs(1), "100 s of audio took {elapsed:?}");
}
