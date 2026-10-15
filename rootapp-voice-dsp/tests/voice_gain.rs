#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::disallowed_macros,
    clippy::arithmetic_side_effects
)]

mod support;

use rootapp_voice_dsp::voice_gain::{VoiceGain, VoiceGainOptions, VoiceGainTuning};
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

/// One syllable frame of the 220 Hz tone at `level_db` (frame `f` of the stream) with `probability`.
fn syllable(gain: &mut VoiceGain, level_db: f64, f: usize, probability: f32) -> f32 {
    let a = amplitude(level_db);
    let mut frame = [0.0f32; FRAME];
    for (i, s) in frame.iter_mut().enumerate() {
        *s = a * (2.0 * PI * 220.0 * (f * FRAME + i) as f32 / RATE).sin();
    }
    gain.process_with_probability(&mut frame, probability);
    frame.iter().fold(0.0f32, |m, &s| m.max(s.abs()))
}

fn silence(gain: &mut VoiceGain, probability: f32) {
    gain.process_with_probability(&mut [0.0f32; FRAME], probability);
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
    talk_quiet_room(&mut gain, -35.0, 6.0);
    assert!(in_range(gain.gain_db(), 9.0, 10.5), "{}", gain.gain_db());
    assert!(gain.is_settled());
}

#[test]
fn a_very_quiet_talker_gets_at_most_the_maximum_boost() {
    let mut gain = new_gain(0.0);
    talk_quiet_room(&mut gain, -56.0, 12.0);
    assert_eq!(VoiceGainOptions::default().max_gain_db, 24.0);
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

/// The clicks cap the gain like noise does, at the fall rate (2 dB/s once settled).
#[test]
fn clicks_in_a_pause_cap_a_quiet_talkers_gain() {
    let mut gain = new_gain(0.0);
    talk_quiet_room(&mut gain, -46.0, 6.0);
    let talking = gain.gain_db();

    let mut frame = [0.0f32; FRAME];
    let mut previous = talking;
    for f in 0..600 {
        if f % 25 == 0 {
            click(&mut frame, 0.2);
        } else {
            frame.fill(0.0);
        }
        gain.process(&mut frame);
        assert!(previous - gain.gain_db() <= 0.02 + 1e-9, "fell faster than 2 dB/s at {f}");
        previous = gain.gain_db();
    }

    assert!(in_range(talking, 14.9, VoiceGainOptions::default().max_gain_db), "{talking}");
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

/// The limiter clamps a burst at once and lets go over about 100 ms (1 dB a frame), never letting
/// a peak over the ceiling.
#[test]
fn the_limiter_attacks_at_once_and_releases_one_db_a_frame() {
    let mut gain = new_gain(12.0);
    let ceiling = VoiceGainOptions::default().limiter_ceiling;
    let mut limits = Vec::new();
    for f in 0..40 {
        let level = if (10..13).contains(&f) { -6.0 } else { -24.0 };
        let peak = syllable(&mut gain, level, f, -1.0);
        assert!(peak <= ceiling + 1e-4, "frame {f}: {peak}");
        limits.push(f64::from(gain.limiter_gain()));
    }
    assert_eq!(limits[9], 1.0);
    let attacked = 20.0 * limits[10].log10();
    assert!(attacked < -8.0, "{attacked}");
    for f in 13..30 {
        let step = 20.0 * (limits[f] / limits[f - 1]).log10();
        assert!(step <= 1.0 + 1e-4, "frame {f}: +{step} dB");
    }
    assert!(limits[13] < 1.0 && limits[39] == 1.0);
}

#[test]
fn starting_from_the_learned_gain_the_first_seconds_match_the_rest() {
    let mut learning = new_gain(0.0);
    talk_quiet_room(&mut learning, -36.0, 8.0);
    assert!(learning.is_settled());

    let mut next_call = new_gain(learning.gain_db());
    let mut levels = Vec::new();
    talk(&mut next_call, -36.0, 8.0, -95.0, Some(&mut levels));
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
    room_talk_with(gain, voice_db, &|_| room_db, seconds, other_db);
}

/// `room_db(f)`: the room's level on frame f. Returns the gain after every frame.
fn room_talk_with(
    gain: &mut VoiceGain,
    voice_db: Option<f64>,
    room_db: &dyn Fn(usize) -> f64,
    seconds: f64,
    other_db: Option<f64>,
) -> Vec<f64> {
    let mut gains = Vec::new();
    let mut input = [0.0f32; FRAME];
    let mut output = [0.0f32; FRAME];
    let mut rng = Rng::new(5);
    let residue = (10f64.powf(-85.0 / 20.0) * 3f64.sqrt()) as f32;
    for f in 0..(seconds * 100.0) as usize {
        let room = (10f64.powf(room_db(f) / 20.0) * 3f64.sqrt()) as f32;
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
        gains.push(gain.gain_db());
    }
    gains
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
    assert!(in_range(gain.gain_db(), 12.5, 13.5), "{}", gain.gain_db());
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
    assert!(in_range(gain.gain_db(), 4.0, 6.0), "{}", gain.gain_db());

    room_talk(&mut gain, None, -80.0, 12.0, Some(-50.0));

    assert!(in_range(gain.speech_level_db(), speech_level - 1.0, speech_level + 1.0));
    assert!(in_range(gain.room_noise_db(), -56.0, -49.0), "{}", gain.room_noise_db());
    let cap = VoiceGainOptions::default().max_room_noise_db - gain.room_noise_db() + 0.1;
    assert!(in_range(gain.gain_db(), 0.0, cap), "{} vs {cap}", gain.gain_db());
}

#[test]
fn a_remembered_gain_falls_in_a_noisy_room_before_anyone_speaks() {
    let mut gain = new_gain(VoiceGainOptions::default().max_gain_db);
    room_talk(&mut gain, None, -45.0, 6.0, None);
    assert_eq!(gain.speech_ms(), 0.0);
    assert!(near(gain.gain_db(), 0.0, 0.05), "{}", gain.gain_db());
}

#[test]
fn a_remembered_gain_is_kept_in_a_quiet_room_until_the_person_speaks() {
    let mut gain = new_gain(12.0);
    room_talk(&mut gain, None, -80.0, 4.0, None);
    assert!(near(gain.gain_db(), 12.0, 1e-6), "{}", gain.gain_db());
}

/// A door slam in a pause, 30 dB over the room for 300 ms, moves the room estimate by under 2 dB
/// (each committed frame counts at most 6 dB over it), so the cap does not pull the gain down; the
/// old unclipped average rose 26 dB and held the gain at 0 dB for seconds.
#[test]
fn a_noise_burst_in_a_pause_barely_moves_the_room() {
    let mut gain = new_gain(0.0);
    room_talk(&mut gain, Some(-38.0), -66.0, 8.0, None);
    let before = gain.room_noise_db();
    let talking = gain.gain_db();
    assert!(in_range(talking, 12.5, 13.5), "{talking}");
    room_talk_with(
        &mut gain,
        None,
        &|f| if (50..80).contains(&f) { -36.0 } else { -66.0 },
        2.0,
        None,
    );
    assert!(gain.room_noise_db() - before < 2.0, "{before} -> {}", gain.room_noise_db());
    assert!(gain.gain_db() > talking - 0.5, "{talking} -> {}", gain.gain_db());
}

/// A room that gets louder for good (a fan turning up) still caps the gain, which comes down no
/// faster than the fall rate. The room is only measured in pauses, at most 6 dB over its estimate
/// per frame: in this talk (a 0.5 s pause every 1.5 s) the estimate rises about 0.5 dB/s, in
/// silence 6.5 dB/s. A room that jumps at once counts only when the input's floor has followed it
/// (6 dB/s): 21 dB are learned about 3.5 s later than the 6.5 dB/s alone would.
#[test]
fn a_rising_room_caps_the_gain_at_the_fall_rate() {
    let mut gain = new_gain(0.0);
    room_talk(&mut gain, Some(-38.0), -75.0, 8.0, None);
    assert!(in_range(gain.gain_db(), 12.5, 13.5), "{}", gain.gain_db());
    let start = gain.gain_db();
    let mut gains = room_talk_with(
        &mut gain,
        Some(-38.0),
        &|f| (-75.0 + 0.02 * f as f64).min(-54.0),
        60.0,
        None,
    );
    let talked = gain.room_noise_db();
    gains.extend(room_talk_with(&mut gain, None, &|_| -54.0, 3.0, None));
    let fastest = gains
        .iter()
        .scan(start, |previous, &g| {
            let fell = *previous - g;
            *previous = g;
            Some(fell)
        })
        .fold(0.0f64, f64::max);
    assert!(in_range(talked, -58.0, -53.0), "{talked}");
    let limit = VoiceGainOptions::default().max_room_noise_db - gain.room_noise_db();
    assert!(in_range(gain.room_noise_db(), -55.0, -53.0), "{}", gain.room_noise_db());
    assert!(in_range(gain.gain_db(), limit - 0.5, limit + 0.1), "{} vs {limit}", gain.gain_db());
    assert!(fastest <= 0.02 + 1e-9, "fell {fastest} dB in a frame");

    let mut silent = new_gain(0.0);
    room_talk(&mut silent, None, -75.0, 3.0, None);
    room_talk(&mut silent, None, -54.0, 3.0, None);
    assert!(
        silent.room_noise_db() < -70.0,
        "the input's floor is still rising: {}",
        silent.room_noise_db()
    );
    room_talk(&mut silent, None, -54.0, 5.0, None);
    assert!(in_range(silent.room_noise_db(), -55.5, -53.0), "{}", silent.room_noise_db());
}

/// The hw.12 dip: speech the denoiser erased (loud before it, silence after) used to count as the
/// room, which then capped the gain near 0 dB for as long as the pause that followed. Erased speech
/// is no room now, and the gain stays where the talker's level put it.
#[test]
fn speech_the_denoiser_erased_is_never_the_room() {
    let mut gain = new_gain(0.0);
    room_talk(&mut gain, Some(-40.0), -75.0, 8.0, None);
    let talking = gain.gain_db();
    let room = gain.room_noise_db();
    assert!(in_range(talking, 13.0, 16.0), "{talking}");

    let mut input = [0.0f32; FRAME];
    let mut rng = Rng::new(8);
    let residue = (10f64.powf(-85.0 / 20.0) * 3f64.sqrt()) as f32;
    let floor = (10f64.powf(-75.0 / 20.0) * 3f64.sqrt()) as f32;
    for f in 0..1000 {
        let voiced = f % 150 < 100 && f % 30 < 20;
        let a = if voiced { amplitude(-40.0) } else { 0.0 };
        for (i, s) in input.iter_mut().enumerate() {
            let n = (f * FRAME + i) as f32;
            *s = a * (2.0 * PI * 220.0 * n / RATE).sin() + floor * rng.symmetric();
        }
        let mut output: Vec<f32> = (0..FRAME).map(|_| residue * rng.symmetric()).collect();
        gain.observe_input(&input);
        gain.process(&mut output);
    }
    assert!(gain.room_noise_db() - room < 1.0, "{room} -> {}", gain.room_noise_db());
    assert!(gain.gain_db() > talking - 0.5, "{talking} -> {}", gain.gain_db());
}

#[test]
fn the_limit_says_what_holds_the_gain() {
    use rootapp_voice_dsp::GainLimit;
    let mut gain = new_gain(6.0);
    assert_eq!(gain.limit(), GainLimit::None);
    talk_quiet_room(&mut gain, -35.0, 6.0);
    assert_eq!(gain.limit(), GainLimit::Target);
    let mut quiet = new_gain(0.0);
    talk_quiet_room(&mut quiet, -56.0, 6.0);
    assert_eq!(quiet.limit(), GainLimit::Max);

    let mut noisy = new_gain(0.0);
    talk(&mut noisy, -42.0, 8.0, -62.0, None);
    assert_eq!(noisy.limit(), GainLimit::Noise);

    let mut room = new_gain(0.0);
    room_talk(&mut room, Some(-38.0), -58.0, 8.0, None);
    assert_eq!(room.limit(), GainLimit::Room);
    let mut before_speech = new_gain(VoiceGainOptions::default().max_gain_db);
    room_talk(&mut before_speech, None, -45.0, 3.0, None);
    assert_eq!(before_speech.limit(), GainLimit::Room);
    assert_eq!(
        [GainLimit::None, GainLimit::Target, GainLimit::Max, GainLimit::Noise, GainLimit::Room]
            .map(GainLimit::to_wire),
        [0, 1, 2, 3, 4]
    );
}

/// The talker's level is the average of their frames in dB, not of their power: syllables at -30
/// and -40 dBFS are -35 dB (the power average would be -32.6).
#[test]
fn the_speech_level_is_the_average_frame_level_in_db() {
    let mut gain = new_gain(0.0);
    for f in 0..1500 {
        match f % 30 {
            0..=9 => syllable(&mut gain, -30.0, f, -1.0),
            10..=19 => syllable(&mut gain, -40.0, f, -1.0),
            _ => {
                silence(&mut gain, -1.0);
                0.0
            }
        };
    }
    assert!(in_range(gain.speech_level_db(), -35.6, -34.4), "{}", gain.speech_level_db());
}

/// A 1.5 s laugh 14 dB over a steady talker mid-sentence moves the gain by under 3 dB (the level
/// is a 4 s average of frames in dB, and the gain falls at 2 dB/s): the old 1.5 s power average
/// took 8 dB off for seconds.
#[test]
fn a_loud_laugh_mid_sentence_barely_moves_the_gain() {
    let mut gain = new_gain(0.0);
    let mut f = 0;
    let mut speak = |gain: &mut VoiceGain, seconds: f64, level: &dyn Fn(usize) -> f64| {
        for _ in 0..(seconds * 100.0) as usize {
            if f % 30 < 20 {
                syllable(gain, level(f), f, 0.95);
            } else {
                silence(gain, 0.05);
            }
            f += 1;
        }
    };
    speak(&mut gain, 15.0, &|_| -34.0);
    let before = gain.gain_db();
    assert!(in_range(before, 8.5, 9.5), "{before}");
    let mut lowest = before;
    speak(&mut gain, 1.5, &|f| if f % 20 < 9 { -20.0 } else { -60.0 });
    lowest = lowest.min(gain.gain_db());
    for _ in 0..20 {
        speak(&mut gain, 0.5, &|_| -34.0);
        lowest = lowest.min(gain.gain_db());
    }
    assert!(before - lowest < 3.0, "{before} -> {lowest}");
    assert!(near(gain.gain_db(), before, 0.6), "{before} -> {}", gain.gain_db());
}

/// With a speech probability, only frames at the minimum (0.5) or above are speech: a sound the
/// detector rejects never sets the level or lifts the gain; weighted by the probability, a 0.5
/// frame counts half as much as a certain one.
#[test]
fn only_frames_the_detector_rates_as_speech_set_the_level() {
    let mut rejected = new_gain(0.0);
    for f in 0..600 {
        if f % 30 < 20 {
            syllable(&mut rejected, -40.0, f, 0.49);
        } else {
            silence(&mut rejected, 0.0);
        }
    }
    assert_eq!(rejected.speech_ms(), 0.0);
    assert_eq!(rejected.speech_level_db(), f64::NEG_INFINITY);
    assert_eq!(rejected.gain_db(), 0.0);

    let mut weighted = new_gain(0.0);
    for _ in 0..100 {
        silence(&mut weighted, 0.0);
    }
    for f in 0..300 {
        let (level, p) = if f % 2 == 0 { (-30.0, 1.0) } else { (-40.0, 0.5) };
        syllable(&mut weighted, level, f, p);
    }
    let expected = (-30.0 * 1.0 + -40.0 * 0.5) / 1.5;
    assert!(near(weighted.speech_level_db(), expected, 0.2), "{}", weighted.speech_level_db());

    let mut level_only = new_gain(0.0);
    let mut nan = new_gain(0.0);
    for _ in 0..100 {
        silence(&mut level_only, -1.0);
        silence(&mut nan, f32::NAN);
    }
    for f in 0..300 {
        syllable(&mut level_only, -30.0, f, -1.0);
        syllable(&mut nan, -30.0, f, f32::NAN);
    }
    assert_eq!(level_only.speech_level_db(), nan.speech_level_db());
    assert!(near(level_only.speech_level_db(), -30.0, 0.1), "{}", level_only.speech_level_db());
}

/// Silence holds the gain: an hour-long pause neither lowers nor raises it.
#[test]
fn a_long_silence_holds_the_gain() {
    let mut gain = new_gain(0.0);
    room_talk(&mut gain, Some(-34.0), -75.0, 10.0, None);
    let talking = gain.gain_db();
    assert!(in_range(talking, 8.0, 10.0), "{talking}");
    room_talk(&mut gain, None, -75.0, 60.0, None);
    assert_eq!(gain.gain_db(), talking);
}

/// The fast phase before SettledSpeechMs of speech moves at 6 dB/s, never past the gain it
/// settles at by more than 1 dB.
#[test]
fn the_start_moves_at_six_db_a_second_without_overshooting() {
    let mut gain = new_gain(0.0);
    let mut trace = Vec::new();
    let mut previous = 0.0;
    for f in 0..2000 {
        if f % 30 < 20 {
            syllable(&mut gain, -33.0, f, -1.0);
        } else {
            silence(&mut gain, -1.0);
        }
        assert!(gain.gain_db() - previous <= 0.06 + 1e-9, "frame {f}");
        previous = gain.gain_db();
        trace.push(gain.gain_db());
    }
    let settled = trace[1900];
    assert!(in_range(settled, 7.5, 8.5), "{settled}");
    let peak = trace.iter().cloned().fold(f64::MIN, f64::max);
    assert!(peak - settled <= 1.0, "{peak} vs {settled}");
}

#[test]
fn tuning_applies_on_the_next_frame_and_a_reset_clamps_to_the_maximum() {
    let mut gain = new_gain(0.0);
    gain.set_tuning(&VoiceGainTuning { max_gain_db: 15.0, ..VoiceGainTuning::UNSET });
    talk_quiet_room(&mut gain, -46.0, 6.0);
    assert!(near(gain.gain_db(), 15.0, 1e-3));
    gain.set_tuning(&VoiceGainTuning { max_gain_db: 6.0, ..VoiceGainTuning::UNSET });
    talk_quiet_room(&mut gain, -46.0, 1.0);
    assert!(in_range(gain.gain_db(), 12.9, 13.1), "falls at 2 dB/s: {}", gain.gain_db());
    talk_quiet_room(&mut gain, -46.0, 4.0);
    assert!(near(gain.gain_db(), 6.0, 1e-6), "{}", gain.gain_db());
    gain.reset(30.0);
    assert_eq!(gain.gain_db(), 6.0);

    gain.set_tuning(&VoiceGainTuning {
        max_gain_db: 40.0,
        target_db: -100.0,
        ..VoiceGainTuning::UNSET
    });
    assert_eq!(gain.options().max_gain_db, 24.0);
    assert_eq!(gain.options().target_db, -40.0);
    gain.set_tuning(&VoiceGainTuning::UNSET);
    assert_eq!(*gain.options(), VoiceGainOptions::default());
}

#[test]
fn tunables_are_clamped_and_non_finite_ones_are_the_defaults() {
    let d = VoiceGainTuning::DEFAULT;
    assert_eq!(VoiceGainTuning::UNSET.sanitized(), d);
    let unset = VoiceGainTuning::default();
    assert!(
        unset.max_gain_db.is_nan() && unset.target_db.is_nan() && unset.rise_db_per_second.is_nan()
    );
    assert!(unset.fall_db_per_second.is_nan() && unset.level_time_constant_ms.is_nan());
    assert!(unset.min_speech_probability.is_nan());
    assert_eq!((d.max_gain_db, d.target_db), (24.0, -25.0));
    assert_eq!((d.rise_db_per_second, d.fall_db_per_second), (3.0, 2.0));
    assert_eq!((d.level_time_constant_ms, d.min_speech_probability), (4000.0, 0.5));
    let low = VoiceGainTuning {
        max_gain_db: -1.0,
        target_db: -90.0,
        rise_db_per_second: 0.0,
        fall_db_per_second: -3.0,
        level_time_constant_ms: 1.0,
        min_speech_probability: -0.5,
    };
    assert_eq!(low.sanitized(), VoiceGainTuning::MIN);
    let high = VoiceGainTuning {
        max_gain_db: 100.0,
        target_db: 0.0,
        rise_db_per_second: 1e9,
        fall_db_per_second: 99.0,
        level_time_constant_ms: 1e9,
        min_speech_probability: 2.0,
    };
    assert_eq!(high.sanitized(), VoiceGainTuning::MAX);
    let infinite = VoiceGainTuning {
        max_gain_db: f32::INFINITY,
        target_db: f32::NEG_INFINITY,
        ..VoiceGainTuning::DEFAULT
    };
    assert_eq!(infinite.sanitized(), d);
    assert_eq!(VoiceGainTuning::MAX.max_gain_db, 24.0);
}

#[test]
fn reset_starts_over_from_the_given_gain() {
    let mut gain = new_gain(0.0);
    talk_quiet_room(&mut gain, -46.0, 5.0);
    gain.reset(3.0);
    assert!(near(gain.gain_db(), 3.0, 1e-6));
    assert!(!gain.is_settled());
    assert_eq!(gain.speech_ms(), 0.0);
    assert_eq!(gain.speech_level_db(), f64::NEG_INFINITY);
    assert_eq!(gain.limiter_gain(), 1.0);
}

#[test]
fn start_gains_are_clamped_and_nan_is_zero() {
    assert_eq!(new_gain(f64::NAN).gain_db(), 0.0);
    assert_eq!(new_gain(-4.0).gain_db(), 0.0);
    assert_eq!(new_gain(f64::INFINITY).gain_db(), 24.0);
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
    gain.process_with_probability(&mut [], 0.9);
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
        gain.process_with_probability(&mut frame, 0.9);
    }
    let elapsed = clock.elapsed();
    assert!(elapsed < std::time::Duration::from_secs(1), "100 s of audio took {elapsed:?}");
}
