#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::disallowed_macros,
    clippy::arithmetic_side_effects
)]

use rootapp_voice_dsp::frame_delay::{FrameDelay, FrameTag};

const HELD: FrameTag = FrameTag { held: true, open: true, speech: false };
const LIVE: FrameTag = FrameTag { held: false, open: true, speech: true };

fn frame(k: usize, len: usize) -> Vec<f32> {
    vec![k as f32 + 1.0; len]
}

#[test]
fn a_frame_comes_out_the_set_number_of_frames_later_with_its_tag() {
    for frames in 1..=6 {
        let mut d = FrameDelay::new(6, 480);
        assert_eq!(d.restart(frames, 480, HELD), frames);
        let mut out = vec![9.0f32; 480];
        for k in 0..40 {
            let tag = FrameTag { speech: k % 3 == 0, ..LIVE };
            let older = d.exchange(&frame(k, 480), &mut out, tag);
            if k < frames {
                assert_eq!(out, vec![0.0; 480], "{frames}: silence first");
                assert_eq!(older, HELD, "{frames}: tagged as it started");
            } else {
                assert_eq!(out, frame(k - frames, 480), "{frames}: frame {k}");
                assert_eq!(older.speech, (k - frames) % 3 == 0);
            }
        }
    }
}

#[test]
fn a_restart_drops_what_was_in_flight_and_a_clear_stops_it() {
    let mut d = FrameDelay::new(6, 480);
    d.restart(4, 480, LIVE);
    let mut out = vec![0.0f32; 480];
    for k in 0..10 {
        d.exchange(&frame(k, 480), &mut out, LIVE);
    }
    d.restart(2, 480, LIVE);
    for k in 10..12 {
        d.exchange(&frame(k, 480), &mut out, LIVE);
        assert_eq!(out, vec![0.0; 480], "frames 6..9 are gone");
    }
    d.exchange(&frame(12, 480), &mut out, LIVE);
    assert_eq!(out, frame(10, 480));
    d.clear();
    assert_eq!(d.frames(), 0);
    let tag = FrameTag { held: true, open: false, speech: true };
    assert_eq!(d.exchange(&frame(13, 480), &mut out, tag), tag, "inactive: the frame itself");
    assert_eq!(out, frame(13, 480));
}

#[test]
fn shapes_it_cannot_hold_pass_through_and_restarts_are_bounded() {
    let mut d = FrameDelay::new(6, 480);
    assert_eq!(d.restart(9, 480, LIVE), 6, "clamped to the allocation");
    assert_eq!(d.restart(4, 481, LIVE), 0, "longer than a slot");
    assert_eq!(d.restart(4, 0, LIVE), 0);
    assert_eq!(d.restart(0, 480, LIVE), 0);
    d.restart(3, 160, LIVE);
    let mut out = vec![0.0f32; 480];
    assert_eq!(d.exchange(&frame(1, 480), &mut out, HELD), HELD, "another length passes");
    assert_eq!(out, frame(1, 480));
    let mut short = vec![0.0f32; 100];
    d.exchange(&frame(2, 160), &mut short, LIVE);
    assert_eq!(&short[..], &frame(2, 160)[..100], "an output of another length gets what fits");
    let mut out = vec![0.0f32; 160];
    d.exchange(&frame(3, 160), &mut out, LIVE);
    assert_eq!(out, vec![0.0; 160], "the 160-sample delay was not moved by them");
    let mut empty = FrameDelay::new(0, 0);
    assert_eq!(empty.restart(4, 1, LIVE), 0);
    assert_eq!(empty.exchange(&[1.0], &mut [0.0], LIVE), LIVE);
}
