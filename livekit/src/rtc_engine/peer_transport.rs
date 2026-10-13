// Copyright 2025 LiveKit, Inc.
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

use std::{
    collections::{HashMap, HashSet},
    fmt::{Debug, Formatter},
    sync::Arc,
};

use libwebrtc::{peer_connection::BitrateSettings, prelude::*};
use livekit_protocol as proto;
use parking_lot::Mutex;
use tokio::sync::Mutex as AsyncMutex;

use super::EngineResult;

pub type OnOfferCreated = Box<dyn FnMut(SessionDescription) + Send + Sync>;

struct TransportInner {
    pending_candidates: Vec<IceCandidate>,
    renegotiate: bool,
    restarting_ice: bool,
    single_pc_mode: bool,
    pending_initial_offer: Option<SessionDescription>,
    stereo_track_ids: HashSet<String>,
    external_track_ids: HashSet<String>,
    last_offer_sdp: Option<String>,
    send_codec_pins: HashMap<String, SendCodecPin>,
}

/// The codec lines the first answer gave an audio send section (by mid), for the track it sends.
#[derive(Clone, Debug)]
struct SendCodecPin {
    track_id: String,
    formats: Vec<String>,
    opus_fmtp: Vec<String>,
    reported: bool,
}

#[derive(Default)]
struct BitratePreferences {
    start_applied: bool,
    // Sum of the encodings' max bitrates of each published video track, by track id.
    video_max_bps: HashMap<String, u64>,
}

impl BitratePreferences {
    /// libwebrtc's probe ceiling when no max bitrate is set.
    const DEFAULT_MAX_PROBE_BPS: u64 = 5_000_000;

    /// The settings to apply for a newly published video track. Applying a start bitrate
    /// resets the transport's bandwidth estimate to it, so only the first video track that
    /// has one sets it.
    fn on_video_track(
        &mut self,
        track_id: String,
        max_bps: Option<u64>,
        screen: bool,
    ) -> BitrateSettings {
        if let Some(bps) = max_bps {
            self.video_max_bps.insert(track_id, bps);
        }
        let start_kbps = if self.start_applied {
            None
        } else {
            PeerTransport::compute_start_bitrate_kbps(max_bps, screen)
        };
        self.start_applied |= start_kbps.is_some();
        BitrateSettings {
            start_bitrate_bps: start_kbps.map(|kbps| kbps as i32 * 1000),
            max_bitrate_bps: Some(self.max_bitrate_bps()),
            ..Default::default()
        }
    }

    fn on_video_track_removed(&mut self, track_id: &str) -> Option<BitrateSettings> {
        self.video_max_bps.remove(track_id)?;
        Some(BitrateSettings {
            max_bitrate_bps: Some(self.max_bitrate_bps()),
            ..Default::default()
        })
    }

    /// Without a max bitrate libwebrtc caps every bandwidth probe at 5 Mbps. With periodic ALR
    /// probing instead of padding, that holds the estimate near 5-6 Mbps while the content is
    /// static, and a 15 Mbps top layer then needs ~15 s of AIMD increase once it moves. Twice
    /// the published video's max bitrates matches the ceiling libwebrtc puts on probes once
    /// that video is allocated (twice the max allocated bitrate).
    fn max_bitrate_bps(&self) -> i32 {
        let max = self.video_max_bps.values().fold(0u64, |sum, bps| sum.saturating_add(*bps));
        let max = max.saturating_mul(2);
        max.clamp(Self::DEFAULT_MAX_PROBE_BPS, i32::MAX as u64) as i32
    }
}

pub struct PeerTransport {
    signal_target: proto::SignalTarget,
    peer_connection: PeerConnection,
    on_offer_handler: Mutex<Option<OnOfferCreated>>,
    inner: Arc<AsyncMutex<TransportInner>>,
    bitrate: Mutex<BitratePreferences>,
    released_send_mids: Mutex<HashSet<String>>,
}

impl Debug for PeerTransport {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        f.debug_struct("PeerTransport").field("target", &self.signal_target).finish()
    }
}

impl PeerTransport {
    pub fn new(
        peer_connection: PeerConnection,
        signal_target: proto::SignalTarget,
        single_pc_mode: bool,
    ) -> Self {
        Self {
            signal_target,
            peer_connection,
            on_offer_handler: Mutex::new(None),
            inner: Arc::new(AsyncMutex::new(TransportInner {
                pending_candidates: Vec::default(),
                renegotiate: false,
                restarting_ice: false,
                single_pc_mode,
                pending_initial_offer: None,
                stereo_track_ids: HashSet::new(),
                external_track_ids: HashSet::new(),
                last_offer_sdp: None,
                send_codec_pins: HashMap::new(),
            })),
            bitrate: Mutex::new(BitratePreferences::default()),
            released_send_mids: Mutex::new(HashSet::new()),
        }
    }

    /// Leaves the sender's stream out of the m-section `mid` in every later offer, once its
    /// track was removed.
    ///
    /// JSEP keeps a sender's msid and SSRCs in its m-section once it has sent, so the
    /// transceiver's media channel would keep the send stream, and with it an encoder task
    /// queue thread, until the PeerConnection closes. Without the stream in the local
    /// description, libwebrtc removes it. Stopping the transceiver would also free it, but LiveKit
    /// server 1.13 cannot map simulcast layers onto the m-section libwebrtc then recycles.
    pub fn release_send_section(&self, mid: String) {
        self.released_send_mids.lock().insert(mid);
    }

    pub fn is_connected(&self) -> bool {
        self.peer_connection.connection_state() == PeerConnectionState::Connected
    }

    pub fn peer_connection(&self) -> PeerConnection {
        self.peer_connection.clone()
    }

    pub fn signal_target(&self) -> proto::SignalTarget {
        self.signal_target
    }

    pub fn on_offer(&self, handler: Option<OnOfferCreated>) {
        *self.on_offer_handler.lock() = handler;
    }

    pub fn close(&self) {
        if let Ok(mut inner) = self.inner.try_lock() {
            inner.pending_initial_offer = None;
        }
        self.peer_connection.close();
    }

    pub async fn add_ice_candidate(&self, ice_candidate: IceCandidate) -> EngineResult<()> {
        let mut inner = self.inner.lock().await;

        if self.peer_connection.current_remote_description().is_some() && !inner.restarting_ice {
            drop(inner);
            self.peer_connection.add_ice_candidate(ice_candidate).await?;

            return Ok(());
        }

        inner.pending_candidates.push(ice_candidate);
        Ok(())
    }

    pub async fn set_remote_description(
        &self,
        remote_description: SessionDescription,
    ) -> EngineResult<()> {
        let mut inner = self.inner.lock().await;

        if let Some(pending_offer) = inner.pending_initial_offer.take() {
            inner.last_offer_sdp = Some(pending_offer.to_string());
            self.peer_connection.set_local_description(pending_offer).await?;
        }

        let remote_description = if remote_description.sdp_type() == SdpType::Answer {
            Self::pin_answer_send_codecs(&mut inner, remote_description)
        } else {
            remote_description
        };
        self.peer_connection.set_remote_description(remote_description).await?;

        for ic in inner.pending_candidates.drain(..) {
            self.peer_connection.add_ice_candidate(ic).await?;
        }

        inner.restarting_ice = false;

        if inner.renegotiate {
            inner.renegotiate = false;
            // Release the lock before re-entering `create_and_send_offer`, which re-acquires
            // `self.inner`. `tokio::sync::Mutex` is not reentrant, so holding the guard across
            // this call would deadlock the task.
            drop(inner);
            self.create_and_send_offer(OfferOptions::default()).await?;
        }

        Ok(())
    }

    pub async fn create_anwser(
        &self,
        offer: SessionDescription,
        options: AnswerOptions,
    ) -> EngineResult<SessionDescription> {
        let offer_sdp = offer.to_string();
        self.set_remote_description(offer).await?;
        let mut answer = self.peer_connection().create_answer(options).await?;

        let answer_sdp = answer.to_string();
        let stereo_munged = Self::munge_answer_stereo_from_offer(&offer_sdp, &answer_sdp);
        if stereo_munged != answer_sdp {
            match SessionDescription::parse(&stereo_munged, answer.sdp_type()) {
                Ok(parsed) => answer = parsed,
                Err(e) => log::warn!("Failed to parse stereo-munged answer, using original: {e}"),
            }
        }

        self.peer_connection().set_local_description(answer.clone()).await?;

        Ok(answer)
    }

    pub async fn add_stereo_track(&self, track_id: String) {
        self.inner.lock().await.stereo_track_ids.insert(track_id);
    }

    /// An audio track fed by an external source (`RtcAudioSource::Native`); see
    /// `pin_send_codecs`.
    pub async fn add_external_audio_track(&self, track_id: String) {
        self.inner.lock().await.external_track_ids.insert(track_id);
    }

    /// Create an initial offer without setting it as local description.
    /// The offer is stored as pending and will be applied when the server's answer arrives.
    ///
    /// In single PC mode, this initial offer is sent with the JoinRequest before any track
    /// is published, with `inactive→recvonly` munging applied.
    pub async fn create_initial_offer(&self) -> EngineResult<Option<SessionDescription>> {
        let inner = self.inner.lock().await;
        if !inner.single_pc_mode {
            return Ok(None);
        }
        drop(inner);

        let mut offer = self.peer_connection.create_offer(OfferOptions::default()).await?;
        let sdp = offer.to_string();

        // Apply inactive→recvonly munging for single PC mode
        let recvonly_munged = Self::munge_inactive_to_recvonly_for_media(&sdp);
        if recvonly_munged != sdp {
            if let Ok(parsed) = SessionDescription::parse(&recvonly_munged, offer.sdp_type()) {
                offer = parsed;
            }
        }

        self.inner.lock().await.pending_initial_offer = Some(offer.clone());
        Ok(Some(offer))
    }

    pub async fn clear_pending_initial_offer(&self) {
        let mut inner = self.inner.lock().await;
        inner.pending_initial_offer = None;
    }

    /// Called for each published video track with the sum of its encodings' max bitrates.
    ///
    /// The first one with a usable start bitrate sets the transport's start bitrate through
    /// SetBitrate. It used to be munged into the offer as x-google-start-bitrate, but every
    /// send channel applies its codec's start bitrate to the whole call, a channel without one
    /// clears it, and the next different value resets the established estimate, so each later
    /// publish dropped the estimate back to the start bitrate. Every track also raises the
    /// estimate's max (see `BitratePreferences::max_bitrate_bps`).
    pub fn add_video_track(&self, track_id: String, max_bps: Option<u64>, screen: bool) {
        let settings = self.bitrate.lock().on_video_track(track_id, max_bps, screen);
        self.apply_bitrate(settings);
    }

    pub fn remove_video_track(&self, track_id: &str) {
        if let Some(settings) = self.bitrate.lock().on_video_track_removed(track_id) {
            self.apply_bitrate(settings);
        }
    }

    fn apply_bitrate(&self, settings: BitrateSettings) {
        match self.peer_connection.set_bitrate(settings) {
            Ok(()) => log::info!(
                "Publisher bitrate: start {:?} bps, max {:?} bps",
                settings.start_bitrate_bps,
                settings.max_bitrate_bps
            ),
            Err(e) => log::warn!("Failed to apply publisher bitrate {settings:?}: {e:?}"),
        }
    }

    /// Maximum start bitrate (kbps) for cameras.
    /// 1 Mbps is a reasonable ceiling that prevents BWE from starting too aggressively.
    const MAX_START_BITRATE_KBPS: u32 = 1000;

    /// Maximum start bitrate (kbps) for screen shares. livekit-client leaves screen
    /// shares uncapped, but Chromium never applies its value to the H.264 payload type the SFU
    /// answers with, so the browser really starts at libwebrtc's default. 3 Mbps is enough to
    /// enable a 2K share's top layer next to its 1.2 Mbps low layer at the first allocation
    /// without starting far above a typical uplink.
    const MAX_SCREEN_START_BITRATE_KBPS: u32 = 3000;

    /// Compute the start bitrate of a video track.
    ///
    /// Returns min(90% of target, 1 Mbps), or 3 Mbps for screen shares. Returns None if no
    /// target bitrate is set or if the target is too low.
    fn compute_start_bitrate_kbps(target_bps: Option<u64>, screen: bool) -> Option<u32> {
        let target_bps = target_bps?;
        let target_kbps = u32::try_from(target_bps / 1000).unwrap_or(u32::MAX);

        if target_kbps < 300 {
            return None;
        }

        let cap =
            if screen { Self::MAX_SCREEN_START_BITRATE_KBPS } else { Self::MAX_START_BITRATE_KBPS };
        let start_kbps = (target_kbps as f64 * 0.9).round() as u32;
        Some(start_kbps.min(target_kbps).min(cap))
    }

    /// Munge SDP to change a=inactive to a=recvonly for RTP media m-lines in single PC mode.
    /// This is needed because WebRTC can generate inactive direction even when transceivers
    /// were configured as recvonly.
    ///
    /// We intentionally limit this to RTP m-sections, so non-RTP sections (for example
    /// data-channel `m=application` sections) are not rewritten.
    fn munge_inactive_to_recvonly_for_media(sdp: &str) -> String {
        // Detect what line ending the original SDP uses
        let uses_crlf = sdp.contains("\r\n");
        let eol = if uses_crlf { "\r\n" } else { "\n" };

        let lines: Vec<&str> =
            if uses_crlf { sdp.split("\r\n").collect() } else { sdp.split('\n').collect() };

        let mut out: Vec<String> = Vec::with_capacity(lines.len());
        let mut in_rtp_media_section = false;

        for line in lines {
            let l = line.trim();

            // Track whether the current m-section is RTP-based.
            if l.starts_with("m=") {
                // Example RTP m-line:
                //   m=audio 9 UDP/TLS/RTP/SAVPF 111
                // Example data channel m-line:
                //   m=application 9 UDP/DTLS/SCTP webrtc-datachannel
                in_rtp_media_section = l.contains("RTP/");
            }

            // Change inactive to recvonly for RTP media m-sections.
            if in_rtp_media_section && l == "a=inactive" {
                out.push("a=recvonly".to_string());
            } else {
                out.push(line.to_string());
            }
        }

        let mut munged = out.join(eol);
        if !munged.ends_with(eol) {
            munged.push_str(eol);
        }
        munged
    }

    /// Munge SDP to add stereo=1 to opus audio fmtp lines for single PC mode
    /// As per the doc: "In single peer connection mode, the receiver sends the offer,
    /// hence does not know if the sender will send stereo. Therefore, stereo=1 is not set
    /// in the offer. Always set stereo=1 in the offer - This method works."
    fn munge_stereo_for_audio(sdp: &str) -> String {
        // Detect what line ending the original SDP uses
        let uses_crlf = sdp.contains("\r\n");
        let eol = if uses_crlf { "\r\n" } else { "\n" };

        // Split preserving the intended line ending style
        let lines: Vec<&str> =
            if uses_crlf { sdp.split("\r\n").collect() } else { sdp.split('\n').collect() };

        // Find opus payload type (usually 111, but be flexible)
        let mut opus_pts: Vec<&str> = Vec::new();
        for line in &lines {
            let l = line.trim();
            if let Some(rest) = l.strip_prefix("a=rtpmap:") {
                let mut it = rest.split_whitespace();
                let pt = it.next().unwrap_or("");
                let codec = it.next().unwrap_or("");
                // Match opus/48000/2 (stereo opus)
                if codec.starts_with("opus/48000") && !pt.is_empty() {
                    opus_pts.push(pt);
                }
            }
        }

        if opus_pts.is_empty() {
            return sdp.to_string();
        }

        // Rewrite fmtp lines to add stereo=1 if not present
        let mut out: Vec<String> = Vec::with_capacity(lines.len());
        for line in lines {
            let mut rewritten = line.to_string();

            for pt in &opus_pts {
                let prefix = format!("a=fmtp:{pt} ");
                if rewritten.starts_with(&prefix) {
                    // Check if stereo= already exists
                    if !rewritten.contains("stereo=") {
                        // Append stereo=1
                        rewritten.push_str(";stereo=1");
                    }
                    break;
                }
            }

            out.push(rewritten);
        }

        // Re-join using same EOL
        let mut munged = out.join(eol);
        if !munged.ends_with(eol) {
            munged.push_str(eol);
        }
        munged
    }

    fn split_sdp_sections(sdp: &str) -> (&'static str, Vec<Vec<String>>) {
        let eol = if sdp.contains("\r\n") { "\r\n" } else { "\n" };
        let mut sections: Vec<Vec<String>> = vec![Vec::new()];
        for line in sdp.split(eol) {
            if line.starts_with("m=") {
                sections.push(Vec::new());
            }
            sections.last_mut().unwrap().push(line.to_string());
        }
        (eol, sections)
    }

    fn join_sdp_sections(eol: &str, sections: Vec<Vec<String>>) -> String {
        let mut joined = sections.into_iter().flatten().collect::<Vec<_>>().join(eol);
        if !joined.ends_with(eol) {
            joined.push_str(eol);
        }
        joined
    }

    fn opus_payload_types(section: &[String]) -> Vec<String> {
        if !section.first().is_some_and(|m| m.starts_with("m=audio")) {
            return Vec::new();
        }
        section
            .iter()
            .filter_map(|line| {
                let mut it = line.trim().strip_prefix("a=rtpmap:")?.split_whitespace();
                let pt = it.next()?;
                it.next()?.to_ascii_lowercase().starts_with("opus/48000").then(|| pt.to_string())
            })
            .collect()
    }

    fn fmtp_param<'a>(fmtp_line: &'a str, key: &str) -> Option<&'a str> {
        let (_, params) = fmtp_line.trim().split_once(' ')?;
        params.split(';').find_map(|p| {
            let (k, v) = p.trim().split_once('=')?;
            k.trim().eq_ignore_ascii_case(key).then(|| v.trim())
        })
    }

    fn section_has_opus_param(section: &[String], key: &str, value: &str) -> bool {
        Self::opus_payload_types(section).iter().any(|pt| {
            let prefix = format!("a=fmtp:{pt} ");
            section
                .iter()
                .any(|line| line.starts_with(&prefix) && Self::fmtp_param(line, key) == Some(value))
        })
    }

    fn add_opus_stereo(section: &mut [String]) {
        for pt in Self::opus_payload_types(section) {
            let prefix = format!("a=fmtp:{pt} ");
            for line in section.iter_mut() {
                if line.starts_with(&prefix) && Self::fmtp_param(line, "stereo").is_none() {
                    line.push_str(";stereo=1");
                }
            }
        }
    }

    fn section_attribute<'a>(section: &'a [String], attr: &str) -> Option<&'a str> {
        let prefix = format!("a={attr}:");
        section.iter().find_map(|line| line.trim().strip_prefix(prefix.as_str()))
    }

    /// Adds `stereo=1` to the Opus fmtp of publisher m-sections carrying one of `track_ids`
    /// (matched via `a=msid`), so the answer lets libwebrtc open a 2-channel encoder.
    fn munge_stereo_for_tracks(sdp: &str, track_ids: &HashSet<String>) -> String {
        let (eol, mut sections) = Self::split_sdp_sections(sdp);
        for section in sections.iter_mut().skip(1) {
            let is_stereo_track = Self::section_attribute(section, "msid")
                .and_then(|msid| msid.split_whitespace().nth(1))
                .is_some_and(|track_id| track_ids.contains(track_id));
            if is_stereo_track {
                Self::add_opus_stereo(section);
            }
        }
        Self::join_sdp_sections(eol, sections)
    }

    /// Removes the sender's stream (msid, SSRCs, rids, simulcast) from the m-sections in `mids`.
    fn munge_released_send_sections(sdp: &str, mids: &HashSet<String>) -> String {
        const STREAM_ATTRIBUTES: [&str; 5] =
            ["a=msid:", "a=ssrc:", "a=ssrc-group:", "a=rid:", "a=simulcast:"];
        let (eol, mut sections) = Self::split_sdp_sections(sdp);
        for section in sections.iter_mut().skip(1) {
            if Self::section_attribute(section, "mid").is_some_and(|mid| mids.contains(mid)) {
                section.retain(|line| !STREAM_ATTRIBUTES.iter().any(|a| line.starts_with(a)));
            }
        }
        Self::join_sdp_sections(eol, sections)
    }

    /// livekit-client parity (`ensureAudioNackAndStereo`): the subscriber answer gets
    /// `stereo=1` for every mid whose offered Opus fmtp carries `sprop-stereo=1`, otherwise
    /// libwebrtc opens a mono decoder and downmixes stereo publishers.
    fn munge_answer_stereo_from_offer(offer_sdp: &str, answer_sdp: &str) -> String {
        let (_, offer_sections) = Self::split_sdp_sections(offer_sdp);
        let stereo_mids: HashSet<&str> = offer_sections
            .iter()
            .skip(1)
            .filter(|s| Self::section_has_opus_param(s, "sprop-stereo", "1"))
            .filter_map(|s| Self::section_attribute(s, "mid"))
            .collect();
        if stereo_mids.is_empty() {
            return answer_sdp.to_string();
        }

        let (eol, mut sections) = Self::split_sdp_sections(answer_sdp);
        for section in sections.iter_mut().skip(1) {
            let is_stereo_mid = Self::section_attribute(section, "mid")
                .is_some_and(|mid| stereo_mids.contains(mid));
            if is_stereo_mid {
                Self::add_opus_stereo(section);
            }
        }
        Self::join_sdp_sections(eol, sections)
    }

    fn pin_answer_send_codecs(
        inner: &mut TransportInner,
        answer: SessionDescription,
    ) -> SessionDescription {
        let Some(offer_sdp) = inner.last_offer_sdp.as_deref() else {
            return answer;
        };
        let answer_sdp = answer.to_string();
        let pinned = Self::pin_send_codecs(
            offer_sdp,
            &answer_sdp,
            &inner.external_track_ids,
            &inner.stereo_track_ids,
            &mut inner.send_codec_pins,
        );
        if pinned == answer_sdp {
            return answer;
        }
        match SessionDescription::parse(&pinned, answer.sdp_type()) {
            Ok(parsed) => parsed,
            Err(e) => {
                log::warn!("Failed to parse send-codec-pinned answer, using original: {e}");
                answer
            }
        }
    }

    fn m_line_formats(m_line: &str) -> Vec<String> {
        m_line.split_whitespace().skip(3).map(str::to_string).collect()
    }

    fn opus_fmtp_lines(section: &[String]) -> Vec<String> {
        let prefixes: Vec<String> =
            Self::opus_payload_types(section).iter().map(|pt| format!("a=fmtp:{pt} ")).collect();
        section
            .iter()
            .filter(|line| prefixes.iter().any(|prefix| line.starts_with(prefix)))
            .cloned()
            .collect()
    }

    fn send_codec_pin(section: &[String], track_id: &str) -> SendCodecPin {
        SendCodecPin {
            track_id: track_id.to_string(),
            formats: Self::m_line_formats(&section[0]),
            opus_fmtp: Self::opus_fmtp_lines(section),
            reported: false,
        }
    }

    /// Gives an answer section the payload order and Opus fmtp lines of `pin`: payload types both
    /// carry keep the pinned order, any others follow.
    fn apply_send_codec_pin(section: &mut Vec<String>, pin: &SendCodecPin) {
        let answered = Self::m_line_formats(&section[0]);
        let mut formats: Vec<&str> =
            pin.formats.iter().filter(|pt| answered.contains(pt)).map(String::as_str).collect();
        formats.extend(answered.iter().filter(|pt| !pin.formats.contains(pt)).map(String::as_str));
        let head: Vec<&str> = section[0].split_whitespace().take(3).collect();
        section[0] = format!("{} {}", head.join(" "), formats.join(" "));

        let pts = Self::opus_payload_types(section);
        let fmtp_prefixes: Vec<String> = pts.iter().map(|pt| format!("a=fmtp:{pt} ")).collect();
        section.retain(|line| !fmtp_prefixes.iter().any(|prefix| line.starts_with(prefix)));
        for pt in &pts {
            let fmtp_prefix = format!("a=fmtp:{pt} ");
            let rtpmap_prefix = format!("a=rtpmap:{pt} ");
            let Some(rtpmap) = section.iter().position(|line| line.starts_with(&rtpmap_prefix))
            else {
                continue;
            };
            let lines = pin.opus_fmtp.iter().filter(|line| line.starts_with(&fmtp_prefix));
            for (offset, line) in lines.enumerate() {
                section.insert(rtpmap + 1 + offset, line.clone());
            }
        }
    }

    /// Keeps the codec of each audio send section whose track has an external source
    /// (`external_track_ids`, `RtcAudioSource::Native` tracks) as the first answer set it.
    ///
    /// The pinned libwebrtc prebuilt (external_audio_source.patch without its
    /// StoreEncoderProperties fix) registers an external-source send stream, such as screen-share
    /// audio or the managed engine's mic, with AudioState when the stream's send codec (format,
    /// payload type or RED) changes while it sends. With the platform device module recording, the
    /// microphone then reaches that stream next to the source's own frames: twice the audio rate,
    /// garbled at the receiver, and a dangling AudioState entry once the track is unpublished.
    /// LiveKit server's answers do not always give a track the same Opus parameters: it adds
    /// `stereo=1;maxaveragebitrate=510000` only when it resolves the track for that answer, and with
    /// a single peer connection (1.13.7) the first answer for a stereo track lacks
    /// `maxaveragebitrate` and every later one has it. So later answers get the first answer's
    /// payload order and Opus fmtp back. A section whose offer no longer sends that track drops its
    /// pin. A stereo track's first answer gets `stereo=1` when the server left it out.
    ///
    /// A device track (`RtcAudioSource::Device`, the platform engine's mic) is fed through AudioState
    /// anyway, so the bug never reaches it, and its section takes every answer as the server gives
    /// it. Pinned, it kept a first answer that echoed the single-PC offer's `stereo=1` without
    /// `usedtx=1`, and the mic sent stereo Opus with DTX off for the whole call.
    fn pin_send_codecs(
        offer_sdp: &str,
        answer_sdp: &str,
        external_track_ids: &HashSet<String>,
        stereo_track_ids: &HashSet<String>,
        pins: &mut HashMap<String, SendCodecPin>,
    ) -> String {
        let (_, offer_sections) = Self::split_sdp_sections(offer_sdp);
        let sending: HashMap<String, String> = offer_sections
            .iter()
            .skip(1)
            .filter(|s| s.first().is_some_and(|m| m.starts_with("m=audio")))
            .filter(|s| s.iter().any(|line| line == "a=sendonly" || line == "a=sendrecv"))
            .filter_map(|s| {
                let mid = Self::section_attribute(s, "mid")?;
                let track_id = Self::section_attribute(s, "msid")?.split_whitespace().nth(1)?;
                Some((mid.to_string(), track_id.to_string()))
            })
            .filter(|(_, track_id)| external_track_ids.contains(track_id))
            .collect();
        pins.retain(|mid, pin| sending.get(mid) == Some(&pin.track_id));

        let (eol, mut sections) = Self::split_sdp_sections(answer_sdp);
        let mut changed = false;
        for section in sections.iter_mut().skip(1) {
            let Some(mid) = Self::section_attribute(section, "mid").map(str::to_string) else {
                continue;
            };
            let Some(track_id) = sending.get(&mid) else {
                continue;
            };
            let rejected = section[0].split_whitespace().nth(1) == Some("0");
            if rejected || Self::opus_payload_types(section).is_empty() {
                continue;
            }
            match pins.get_mut(&mid) {
                None => {
                    if stereo_track_ids.contains(track_id) {
                        let before = section.clone();
                        Self::add_opus_stereo(section);
                        changed |= *section != before;
                    }
                    pins.insert(mid, Self::send_codec_pin(section, track_id));
                }
                Some(pin) => {
                    let before = section.clone();
                    Self::apply_send_codec_pin(section, pin);
                    if *section != before {
                        let level = if pin.reported { log::Level::Debug } else { log::Level::Warn };
                        pin.reported = true;
                        log::log!(
                            level,
                            "publisher answer changed the send codec of audio mid {mid} (track \
                             {track_id}): [{}] {}; keeping the first answer's [{}] {}",
                            Self::m_line_formats(&before[0]).join(" "),
                            Self::opus_fmtp_lines(&before).join(" | "),
                            pin.formats.join(" "),
                            pin.opus_fmtp.join(" | "),
                        );
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            return answer_sdp.to_string();
        }
        Self::join_sdp_sections(eol, sections)
    }

    pub async fn create_and_send_offer(&self, options: OfferOptions) -> EngineResult<()> {
        let mut inner = self.inner.lock().await;
        if options.ice_restart {
            inner.restarting_ice = true;
        }

        if inner.pending_initial_offer.is_some() {
            inner.renegotiate = true;
            return Ok(());
        }

        if self.peer_connection.signaling_state() == SignalingState::HaveLocalOffer {
            let remote_sdp = self.peer_connection.current_remote_description();
            if options.ice_restart && remote_sdp.is_some() {
                let remote_sdp = remote_sdp.unwrap();

                // Cancel the old renegotiation (Basically say the server rejected the previous
                // offer) So we can resend a new offer just after this
                self.peer_connection.set_remote_description(remote_sdp).await?;
            } else {
                inner.renegotiate = true;
                return Ok(());
            }
        } else if self.peer_connection.signaling_state() == SignalingState::Closed {
            log::warn!("peer connection is closed, cannot create offer");
            return Ok(());
        }

        let mut offer = self.peer_connection.create_offer(options).await?;
        let mut sdp = offer.to_string();

        if inner.single_pc_mode {
            // Fix inactive media m-lines to recvonly for single PC mode.
            // WebRTC can generate a=inactive even when transceivers are recvonly.
            let recvonly_munged = Self::munge_inactive_to_recvonly_for_media(&sdp);
            if recvonly_munged != sdp {
                match SessionDescription::parse(&recvonly_munged, offer.sdp_type()) {
                    Ok(parsed) => {
                        offer = parsed;
                        sdp = recvonly_munged;
                    }
                    Err(e) => {
                        log::warn!("Failed to parse recvonly-munged SDP: {e}");
                    }
                }
            }

            // Apply stereo munging for single PC mode
            let stereo_munged = Self::munge_stereo_for_audio(&sdp);
            if stereo_munged != sdp {
                match SessionDescription::parse(&stereo_munged, offer.sdp_type()) {
                    Ok(parsed) => {
                        offer = parsed;
                        sdp = stereo_munged;
                    }
                    Err(e) => {
                        log::warn!("Failed to parse stereo-munged SDP, using original: {e}");
                    }
                }
            }
        }

        if !inner.stereo_track_ids.is_empty() {
            let stereo_munged = Self::munge_stereo_for_tracks(&sdp, &inner.stereo_track_ids);
            if stereo_munged != sdp {
                match SessionDescription::parse(&stereo_munged, offer.sdp_type()) {
                    Ok(parsed) => {
                        offer = parsed;
                        sdp = stereo_munged;
                    }
                    Err(e) => {
                        log::warn!("Failed to parse stereo-track-munged SDP, using original: {e}");
                    }
                }
            }
        }

        let released_mids = self.released_send_mids.lock().clone();
        if !released_mids.is_empty() {
            let released = Self::munge_released_send_sections(&sdp, &released_mids);
            if released != sdp {
                match SessionDescription::parse(&released, offer.sdp_type()) {
                    Ok(parsed) => offer = parsed,
                    Err(e) => {
                        log::warn!("Failed to parse released-sender SDP, using original: {e}")
                    }
                }
            }
        }

        self.peer_connection.set_local_description(offer.clone()).await?;
        inner.last_offer_sdp = Some(offer.to_string());

        if let Some(handler) = self.on_offer_handler.lock().as_mut() {
            handler(offer);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use libwebrtc::peer_connection::BitrateSettings;

    use super::{BitratePreferences, PeerTransport, SendCodecPin};

    /// Reproduces the publisher-transport self-deadlock.
    ///
    /// `PeerTransport::set_remote_description` locks `self.inner` and then, when
    /// `inner.renegotiate` is set, calls `create_and_send_offer` *while still holding that
    /// guard*. `create_and_send_offer`'s first statement re-locks the same
    /// `tokio::sync::Mutex`, which is not reentrant, so the task waits forever for a lock it
    /// already holds.
    ///
    /// This test drives the production-reachable `HaveLocalOffer` sequence that sets
    /// `renegotiate = true`:
    ///
    ///  1. The publisher creates and sends an ordinary offer, entering `HaveLocalOffer`.
    ///  2. Before the answer is applied, another publish/unpublish negotiation is requested.
    ///     `create_and_send_offer` sees `HaveLocalOffer` and sets `renegotiate = true`.
    ///  3. The answer reaches the transport. Because `renegotiate == true`,
    ///     `set_remote_description` re-enters `create_and_send_offer`; holding the inner lock
    ///     across that call would deadlock.
    ///
    /// The `timeout` distinguishes a deadlock (the future never resolves) from success. On the
    /// buggy code this test fails via the timeout assertion; with the lock released before the
    /// nested call it passes.
    #[tokio::test]
    async fn renegotiation_does_not_deadlock() {
        use std::{
            sync::{Arc, Mutex},
            time::Duration,
        };

        use libwebrtc::prelude::*;
        use livekit_protocol as proto;

        let factory = PeerConnectionFactory::default();
        let mut config = RtcConfiguration::default();
        config.continual_gathering_policy = ContinualGatheringPolicy::GatherOnce;
        config.enable_sctp_snap = true;

        let alice_pc = factory.create_peer_connection(config.clone()).unwrap();
        let bob_pc = factory.create_peer_connection(config).unwrap();

        // Give the publisher an m-line so the offer/answer exchange is non-trivial.
        let _dc = alice_pc.create_data_channel("repro", DataChannelInit::default()).unwrap();

        let transport = PeerTransport::new(
            alice_pc,
            proto::SignalTarget::Publisher,
            /* single_pc_mode= */ true,
        );

        let offers = Arc::new(Mutex::new(Vec::new()));
        let emitted_offers = offers.clone();
        transport.on_offer(Some(Box::new(move |offer| {
            emitted_offers.lock().expect("offers lock poisoned").push(offer);
        })));

        // 1. Send an ordinary publisher offer, putting Alice in `HaveLocalOffer`.
        transport.create_and_send_offer(OfferOptions::default()).await.unwrap();
        assert_eq!(transport.peer_connection().signaling_state(), SignalingState::HaveLocalOffer);

        let offer = offers
            .lock()
            .expect("offers lock poisoned")
            .first()
            .cloned()
            .expect("first offer was not emitted");

        // Bob prepares the answer, but it has not reached Alice yet.
        bob_pc.set_remote_description(offer).await.unwrap();
        let answer = bob_pc.create_answer(AnswerOptions::default()).await.unwrap();
        bob_pc.set_local_description(answer.clone()).await.unwrap();

        // 2. Model another publish/unpublish negotiation during the offer/answer RTT. Because
        //    Alice is still in `HaveLocalOffer`, this only sets `renegotiate = true`.
        transport.create_and_send_offer(OfferOptions::default()).await.unwrap();
        assert_eq!(offers.lock().expect("offers lock poisoned").len(), 1);

        // 3. Applying the answer must complete and emit the deferred follow-up offer.
        let res =
            tokio::time::timeout(Duration::from_secs(10), transport.set_remote_description(answer))
                .await;

        assert!(
            res.is_ok(),
            "DEADLOCK: set_remote_description re-locked the transport's `inner` AsyncMutex while \
             renegotiating (peer_transport.rs: lock at set_remote_description, re-lock at \
             create_and_send_offer)"
        );
        res.unwrap().expect("set_remote_description returned an error");

        assert_eq!(
            offers.lock().expect("offers lock poisoned").len(),
            2,
            "the deferred renegotiation should emit a follow-up offer"
        );
        assert_eq!(transport.peer_connection().signaling_state(), SignalingState::HaveLocalOffer);
    }

    #[test]
    fn inactive_media_is_munged_to_recvonly_for_all_rtp_sections() {
        let sdp = "v=0\n\
o=- 0 0 IN IP4 127.0.0.1\n\
s=-\n\
t=0 0\n\
m=audio 9 UDP/TLS/RTP/SAVPF 111\n\
a=inactive\n\
a=rtpmap:111 opus/48000/2\n\
m=video 9 UDP/TLS/RTP/SAVPF 96\n\
a=inactive\n\
m=text 9 UDP/TLS/RTP/SAVPF 98\n\
a=inactive\n\
m=audio 9 UDP/TLS/RTP/SAVPF 111\n\
a=inactive\n";
        let out = PeerTransport::munge_inactive_to_recvonly_for_media(sdp);
        assert!(out.contains("m=audio 9 UDP/TLS/RTP/SAVPF 111\na=recvonly\n"));
        assert!(out.contains("m=text 9 UDP/TLS/RTP/SAVPF 98\na=recvonly\n"));
        assert_eq!(out.matches("a=recvonly").count(), 4);
        assert_eq!(out.matches("a=inactive").count(), 0);
    }

    #[test]
    fn inactive_application_section_is_not_munged() {
        let sdp = "v=0\n\
o=- 0 0 IN IP4 127.0.0.1\n\
s=-\n\
t=0 0\n\
m=audio 9 UDP/TLS/RTP/SAVPF 111\n\
a=inactive\n\
m=application 9 UDP/DTLS/SCTP webrtc-datachannel\n\
a=inactive\n";
        let out = PeerTransport::munge_inactive_to_recvonly_for_media(sdp);
        assert!(out.contains("m=audio 9 UDP/TLS/RTP/SAVPF 111\na=recvonly\n"));
        assert!(out.contains("m=application 9 UDP/DTLS/SCTP webrtc-datachannel\na=inactive\n"));
    }

    #[test]
    fn stereo_is_added_for_opus_fmtp_only_once() {
        let sdp = "v=0\n\
o=- 0 0 IN IP4 127.0.0.1\n\
s=-\n\
t=0 0\n\
m=audio 9 UDP/TLS/RTP/SAVPF 111 0\n\
a=rtpmap:111 opus/48000/2\n\
a=rtpmap:0 PCMU/8000\n\
a=fmtp:111 minptime=10;useinbandfec=1\n\
a=fmtp:0 foo=bar\n";
        let out = PeerTransport::munge_stereo_for_audio(sdp);
        assert!(out.contains("a=fmtp:111 minptime=10;useinbandfec=1;stereo=1\n"));
        assert!(out.contains("a=fmtp:0 foo=bar\n"));
        assert_eq!(out.matches("stereo=1").count(), 1);
    }

    #[test]
    fn stereo_munging_is_idempotent_when_stereo_already_present() {
        let sdp = "v=0\n\
o=- 0 0 IN IP4 127.0.0.1\n\
s=-\n\
t=0 0\n\
m=audio 9 UDP/TLS/RTP/SAVPF 111\n\
a=rtpmap:111 opus/48000/2\n\
a=fmtp:111 minptime=10;stereo=1\n";
        let out = PeerTransport::munge_stereo_for_audio(sdp);
        assert_eq!(out.matches("stereo=1").count(), 1);
    }

    #[test]
    fn stereo_is_added_only_for_registered_tracks() {
        let sdp = "v=0\r\n\
o=- 0 0 IN IP4 127.0.0.1\r\n\
s=-\r\n\
t=0 0\r\n\
m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n\
a=mid:0\r\n\
a=msid:- mic-track\r\n\
a=rtpmap:111 opus/48000/2\r\n\
a=fmtp:111 minptime=10;useinbandfec=1\r\n\
m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n\
a=mid:1\r\n\
a=msid:- screen-audio\r\n\
a=rtpmap:111 opus/48000/2\r\n\
a=fmtp:111 minptime=10;useinbandfec=1\r\n";
        let ids = ["screen-audio".to_string()].into_iter().collect();
        let out = PeerTransport::munge_stereo_for_tracks(sdp, &ids);
        assert_eq!(out.matches("stereo=1").count(), 1);
        assert!(out.contains(
            "a=msid:- screen-audio\r\na=rtpmap:111 opus/48000/2\r\na=fmtp:111 minptime=10;useinbandfec=1;stereo=1\r\n"
        ));
        assert!(out.ends_with("\r\n") && !out.ends_with("\r\n\r\n"));
    }

    #[test]
    fn camera_start_bitrate_is_capped_at_one_mbps() {
        assert_eq!(PeerTransport::compute_start_bitrate_kbps(Some(15_610_000), false), Some(1000));
        assert_eq!(PeerTransport::compute_start_bitrate_kbps(Some(800_000), false), Some(720));
        assert_eq!(PeerTransport::compute_start_bitrate_kbps(Some(299_000), false), None);
        assert_eq!(PeerTransport::compute_start_bitrate_kbps(None, false), None);
    }

    #[test]
    fn screen_share_start_bitrate_is_capped_at_three_mbps() {
        assert_eq!(PeerTransport::compute_start_bitrate_kbps(Some(16_200_000), true), Some(3000));
        assert_eq!(PeerTransport::compute_start_bitrate_kbps(Some(2_900_000), true), Some(2610));
        assert_eq!(PeerTransport::compute_start_bitrate_kbps(Some(299_000), true), None);
    }

    fn settings(start: Option<i32>, max: i32) -> BitrateSettings {
        BitrateSettings {
            start_bitrate_bps: start,
            max_bitrate_bps: Some(max),
            ..Default::default()
        }
    }

    #[test]
    fn only_the_first_video_track_applies_a_start_bitrate() {
        let mut prefs = BitratePreferences::default();
        assert_eq!(
            prefs.on_video_track("low".into(), Some(200_000), false),
            settings(None, 5_000_000)
        );
        assert_eq!(
            prefs.on_video_track("screen".into(), Some(16_200_000), true),
            settings(Some(3_000_000), 32_800_000)
        );
        assert_eq!(
            prefs.on_video_track("camera".into(), Some(4_110_000), false),
            settings(None, 41_020_000)
        );
        assert_eq!(
            prefs.on_video_track("screen2".into(), Some(16_200_000), true),
            settings(None, 73_420_000)
        );
    }

    #[test]
    fn max_bitrate_follows_the_published_video() {
        let mut prefs = BitratePreferences::default();
        prefs.on_video_track("camera".into(), Some(4_110_000), false);
        prefs.on_video_track("screen".into(), Some(16_200_000), true);
        prefs.on_video_track("unknown".into(), None, false);
        assert_eq!(prefs.on_video_track_removed("unknown"), None);
        assert_eq!(prefs.on_video_track_removed("screen"), Some(settings(None, 8_220_000)));
        assert_eq!(prefs.on_video_track_removed("screen"), None);
        assert_eq!(prefs.on_video_track_removed("camera"), Some(settings(None, 5_000_000)));

        prefs.on_video_track("huge".into(), Some(u64::MAX / 2), true);
        assert_eq!(prefs.max_bitrate_bps(), i32::MAX);
    }

    #[test]
    fn released_send_sections_lose_their_stream() {
        let sdp = "v=0\r\n\
o=- 0 0 IN IP4 127.0.0.1\r\n\
s=-\r\n\
t=0 0\r\n\
a=msid-semantic: WMS\r\n\
m=video 9 UDP/TLS/RTP/SAVPF 96\r\n\
a=mid:1\r\n\
a=inactive\r\n\
a=msid:- camera\r\n\
a=rtpmap:96 VP8/90000\r\n\
a=rid:q send\r\n\
a=rid:f send\r\n\
a=simulcast:send q;f\r\n\
m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n\
a=mid:2\r\n\
a=inactive\r\n\
a=msid:- screen-audio\r\n\
a=rtpmap:111 opus/48000/2\r\n\
a=ssrc:1234 cname:abc\r\n\
m=video 9 UDP/TLS/RTP/SAVPF 96\r\n\
a=mid:3\r\n\
a=sendonly\r\n\
a=msid:- screen\r\n\
a=rtpmap:96 VP8/90000\r\n\
a=ssrc-group:FID 1 2\r\n\
a=ssrc:1 cname:abc\r\n\
a=ssrc:2 cname:abc\r\n";
        let mids = ["1".to_string(), "2".to_string()].into_iter().collect();
        let out = PeerTransport::munge_released_send_sections(sdp, &mids);
        assert!(out.contains(
            "a=mid:1\r\na=inactive\r\na=rtpmap:96 VP8/90000\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111\r\n\
a=mid:2\r\na=inactive\r\na=rtpmap:111 opus/48000/2\r\nm=video"
        ));
        assert!(out.contains("a=msid-semantic: WMS\r\n"));
        assert!(out.ends_with(
            "a=mid:3\r\na=sendonly\r\na=msid:- screen\r\na=rtpmap:96 VP8/90000\r\n\
a=ssrc-group:FID 1 2\r\na=ssrc:1 cname:abc\r\na=ssrc:2 cname:abc\r\n"
        ));
        assert_eq!(PeerTransport::munge_released_send_sections(sdp, &HashSet::new()), sdp);
    }

    #[tokio::test]
    async fn a_released_send_section_drops_its_send_stream() {
        use std::time::{Duration, Instant};

        use libwebrtc::{
            peer_connection_factory::native::PeerConnectionFactoryExt, prelude::*, stats::RtcStats,
            video_source::native::NativeVideoSource,
        };
        use livekit_protocol as proto;

        async fn video_sizes(pc: &PeerConnection) -> Vec<(u32, u32)> {
            pc.get_stats()
                .await
                .unwrap()
                .into_iter()
                .filter_map(|stats| match stats {
                    RtcStats::OutboundRtp(o) if o.stream.kind == "video" => {
                        Some((o.outbound.frame_width, o.outbound.frame_height))
                    }
                    _ => None,
                })
                .collect()
        }

        let factory = PeerConnectionFactory::default();
        let alice = factory.create_peer_connection(RtcConfiguration::default()).unwrap();
        let bob = factory.create_peer_connection(RtcConfiguration::default()).unwrap();
        for (from, to) in [(alice.clone(), bob.clone()), (bob.clone(), alice.clone())] {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<IceCandidate>();
            from.on_ice_candidate(Some(Box::new(move |candidate| {
                let _ = tx.send(candidate);
            })));
            tokio::spawn(async move {
                while let Some(candidate) = rx.recv().await {
                    let _ = to.add_ice_candidate(candidate).await;
                }
            });
        }

        let transport = PeerTransport::new(alice.clone(), proto::SignalTarget::Publisher, false);
        let offers = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let emitted = offers.clone();
        transport.on_offer(Some(Box::new(move |offer| emitted.lock().unwrap().push(offer))));
        let negotiate = || async {
            transport.create_and_send_offer(OfferOptions::default()).await.unwrap();
            let offer = offers.lock().unwrap().pop().unwrap();
            bob.set_remote_description(offer).await.unwrap();
            let answer = bob.create_answer(AnswerOptions::default()).await.unwrap();
            bob.set_local_description(answer.clone()).await.unwrap();
            transport.set_remote_description(answer).await.unwrap();
        };

        let source = NativeVideoSource::new(VideoResolution { width: 640, height: 360 }, false);
        let track = factory.create_video_track("camera", source.clone());
        let send_encodings = [("q", 4.0), ("h", 2.0), ("f", 1.0)]
            .into_iter()
            .map(|(rid, scale)| RtpEncodingParameters {
                rid: rid.to_string(),
                scale_resolution_down_by: Some(scale),
                ..Default::default()
            })
            .collect();
        let transceiver = alice
            .add_transceiver(
                MediaStreamTrack::Video(track),
                RtpTransceiverInit {
                    direction: RtpTransceiverDirection::SendOnly,
                    stream_ids: Vec::new(),
                    send_encodings,
                },
            )
            .unwrap();
        let vp8 = factory
            .get_rtp_sender_capabilities(MediaType::Video)
            .codecs
            .into_iter()
            .filter(|codec| codec.mime_type.eq_ignore_ascii_case("video/vp8"))
            .collect();
        transceiver.set_codec_preferences(vp8).unwrap();
        negotiate().await;

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut timestamp_us = 0;
        while !video_sizes(&alice).await.iter().any(|&(width, _)| width > 0) {
            assert!(Instant::now() < deadline, "the video encoder never produced a frame");
            timestamp_us += 33_333;
            source.capture_frame(&VideoFrame {
                rotation: VideoRotation::VideoRotation0,
                timestamp_us,
                frame_metadata: None,
                buffer: I420Buffer::new(640, 360),
            });
            tokio::time::sleep(Duration::from_millis(33)).await;
        }

        // The stats cache lives 50 ms; each read below must see a fresh report.
        alice.remove_track(transceiver.sender()).unwrap();
        negotiate().await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(!video_sizes(&alice).await.is_empty(), "the send stream survives remove_track");

        transport.release_send_section(transceiver.mid().unwrap());
        for _ in 0..3 {
            negotiate().await;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(video_sizes(&alice).await.is_empty(), "the send stream is gone");
        let sdp = alice.current_local_description().unwrap().to_string();
        assert!(!sdp.contains("a=msid:") && !sdp.contains("a=rid:"), "{sdp}");
        assert_eq!(sdp.matches("m=video").count(), 1);

        alice.close();
        bob.close();
    }

    #[tokio::test]
    async fn video_tracks_never_put_a_start_bitrate_in_the_offer() {
        use libwebrtc::{
            peer_connection_factory::native::PeerConnectionFactoryExt, prelude::*,
            video_source::native::NativeVideoSource,
        };
        use livekit_protocol as proto;

        let factory = PeerConnectionFactory::default();
        let transport = PeerTransport::new(
            factory.create_peer_connection(RtcConfiguration::default()).unwrap(),
            proto::SignalTarget::Publisher,
            false,
        );
        let offers = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let emitted = offers.clone();
        transport.on_offer(Some(Box::new(move |offer| emitted.lock().unwrap().push(offer))));

        for (name, screen) in [("camera", false), ("screen", true)] {
            let source =
                NativeVideoSource::new(VideoResolution { width: 1280, height: 720 }, screen);
            let track = factory.create_video_track(name, source);
            transport.add_video_track(name.to_string(), Some(3_500_000), screen);
            transport
                .peer_connection()
                .add_transceiver(
                    MediaStreamTrack::Video(track),
                    RtpTransceiverInit {
                        direction: RtpTransceiverDirection::SendOnly,
                        stream_ids: Vec::new(),
                        send_encodings: Vec::new(),
                    },
                )
                .unwrap();
        }
        transport.create_and_send_offer(OfferOptions::default()).await.unwrap();

        let offers = offers.lock().unwrap();
        assert_eq!(offers.len(), 1);
        let sdp = offers[0].to_string();
        assert_eq!(sdp.matches("m=video").count(), 2);
        assert!(!sdp.contains("x-google-start-bitrate"));
        assert!(transport.bitrate.lock().start_applied);
    }

    #[test]
    fn answer_gets_stereo_for_sprop_stereo_mids() {
        let offer = "v=0\n\
o=- 0 0 IN IP4 127.0.0.1\n\
s=-\n\
t=0 0\n\
m=audio 9 UDP/TLS/RTP/SAVPF 111\n\
a=mid:0\n\
a=rtpmap:111 opus/48000/2\n\
a=fmtp:111 minptime=10;useinbandfec=1\n\
m=audio 9 UDP/TLS/RTP/SAVPF 111\n\
a=mid:1\n\
a=rtpmap:111 opus/48000/2\n\
a=fmtp:111 minptime=10;sprop-stereo=1;useinbandfec=1\n";
        let answer = "v=0\n\
o=- 0 0 IN IP4 127.0.0.1\n\
s=-\n\
t=0 0\n\
m=audio 9 UDP/TLS/RTP/SAVPF 111\n\
a=mid:0\n\
a=rtpmap:111 opus/48000/2\n\
a=fmtp:111 minptime=10;useinbandfec=1\n\
m=audio 9 UDP/TLS/RTP/SAVPF 111\n\
a=mid:1\n\
a=rtpmap:111 opus/48000/2\n\
a=fmtp:111 minptime=10;useinbandfec=1\n";
        let out = PeerTransport::munge_answer_stereo_from_offer(offer, answer);
        assert_eq!(out.matches(";stereo=1").count(), 1);
        assert!(out.ends_with(
            "a=mid:1\na=rtpmap:111 opus/48000/2\na=fmtp:111 minptime=10;useinbandfec=1;stereo=1\n"
        ));
        assert_eq!(PeerTransport::munge_answer_stereo_from_offer(answer, answer), answer);
    }

    fn audio_sdp(sections: &[(&str, &str, Option<&str>, &str, &str)]) -> String {
        let mut sdp = "v=0\no=- 0 0 IN IP4 127.0.0.1\ns=-\nt=0 0\n".to_string();
        for (mid, direction, track_id, formats, opus_fmtp) in sections {
            sdp.push_str(&format!(
                "m=audio 9 UDP/TLS/RTP/SAVPF {formats}\na=mid:{mid}\na={direction}\n"
            ));
            if let Some(track_id) = track_id {
                sdp.push_str(&format!("a=msid:- {track_id}\n"));
            }
            sdp.push_str(&format!(
                "a=rtpmap:111 opus/48000/2\na=fmtp:111 {opus_fmtp}\na=rtpmap:63 red/48000/2\n\
                 a=fmtp:63 111/111\na=rtpmap:9 G722/8000\n"
            ));
        }
        sdp
    }

    const OFFERED: &str = "minptime=10;useinbandfec=1;stereo=1";
    const RESOLVED: &str = "minptime=10;useinbandfec=1;stereo=1;maxaveragebitrate=510000";
    const UNRESOLVED: &str = "minptime=10;useinbandfec=1;stereo=1";

    fn screen_audio_offer() -> String {
        audio_sdp(&[
            ("0", "recvonly", None, "111 63 9", OFFERED),
            ("1", "sendonly", Some("screen-audio"), "111 63 9", OFFERED),
        ])
    }

    fn answer(mid1_formats: &str, mid1_fmtp: &str) -> String {
        audio_sdp(&[
            ("0", "sendonly", None, "111 63 9", "minptime=10;useinbandfec=1"),
            ("1", "recvonly", None, mid1_formats, mid1_fmtp),
        ])
    }

    const EXTERNAL: [&str; 2] = ["screen-audio", "music"];

    fn ids(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    fn pin(
        offer: &str,
        answer: &str,
        stereo: &[&str],
        pins: &mut HashMap<String, SendCodecPin>,
    ) -> String {
        PeerTransport::pin_send_codecs(offer, answer, &ids(&EXTERNAL), &ids(stereo), pins)
    }

    #[test]
    fn later_answers_keep_the_first_answers_opus_fmtp() {
        let offer = screen_audio_offer();
        let mut pins = HashMap::new();
        let first = answer("111 63 9", RESOLVED);
        assert_eq!(pin(&offer, &first, &[], &mut pins), first);
        assert_eq!(pins.keys().collect::<Vec<_>>(), ["1"]);

        let flipped = answer("111 63 9", UNRESOLVED);
        assert_eq!(pin(&offer, &flipped, &[], &mut pins), first);
        assert_eq!(pin(&offer, &first, &[], &mut pins), first);
    }

    #[test]
    fn later_answers_keep_the_first_answers_payload_order() {
        let offer = screen_audio_offer();
        let mut pins = HashMap::new();
        let first = answer("111 63 9", RESOLVED);
        pin(&offer, &first, &[], &mut pins);

        let red_first = answer("63 111 9", RESOLVED);
        assert_eq!(pin(&offer, &red_first, &[], &mut pins), first);

        let fewer = answer("9 111", UNRESOLVED);
        assert_eq!(pin(&offer, &fewer, &[], &mut pins), answer("111 9", RESOLVED));
    }

    #[test]
    fn only_send_sections_are_pinned_and_a_new_track_is_pinned_again() {
        let offer = screen_audio_offer();
        let mut pins = HashMap::new();
        pin(&offer, &answer("111 63 9", RESOLVED), &[], &mut pins);

        let other_track = offer.replace("a=msid:- screen-audio", "a=msid:- music");
        let unresolved = answer("111 63 9", UNRESOLVED);
        assert_eq!(pin(&other_track, &unresolved, &[], &mut pins), unresolved);
        assert_eq!(pins["1"].track_id, "music");
        assert_eq!(pins["1"].opus_fmtp, [format!("a=fmtp:111 {UNRESOLVED}")]);

        let released = offer.replace("a=msid:- screen-audio\n", "");
        let resolved = answer("111 63 9", RESOLVED);
        assert_eq!(pin(&released, &resolved, &[], &mut pins), resolved);
        assert!(pins.is_empty());

        let mut recv_only = HashMap::new();
        let swapped = answer("111 63 9", RESOLVED)
            .replace("a=mid:0\na=sendonly", "a=mid:0\na=sendonly\na=msid:- remote");
        pin(&offer, &swapped, &[], &mut recv_only);
        assert_eq!(recv_only.keys().collect::<Vec<_>>(), ["1"]);
    }

    #[test]
    fn a_stereo_tracks_first_answer_gets_stereo() {
        let offer = screen_audio_offer();
        let mut pins = HashMap::new();
        let mono = answer("111 63 9", "minptime=10;useinbandfec=1");
        let out = pin(&offer, &mono, &["screen-audio"], &mut pins);
        assert_eq!(out, answer("111 63 9", "minptime=10;useinbandfec=1;stereo=1"));
        assert_eq!(pin(&offer, &answer("111 63 9", RESOLVED), &["screen-audio"], &mut pins), out);

        let mut mono_pins = HashMap::new();
        assert_eq!(pin(&offer, &mono, &[], &mut mono_pins), mono);
    }

    #[test]
    fn pinning_keeps_crlf_and_leaves_unchanged_answers_verbatim() {
        let offer = screen_audio_offer().replace('\n', "\r\n");
        let first = answer("111 63 9", RESOLVED).replace('\n', "\r\n");
        let mut pins = HashMap::new();
        assert_eq!(pin(&offer, &first, &[], &mut pins), first);
        let flipped = answer("111 63 9", UNRESOLVED).replace('\n', "\r\n");
        assert_eq!(pin(&offer, &flipped, &[], &mut pins), first);
    }

    const MIC_FIRST: &str = "minptime=10;useinbandfec=1;stereo=1";
    const MIC_LATER: &str = "minptime=10;useinbandfec=1;usedtx=1";

    #[test]
    fn a_device_tracks_section_is_never_pinned() {
        let offer = screen_audio_offer().replace("a=msid:- screen-audio", "a=msid:- mic");
        let mut pins = HashMap::new();
        for server_answer in [
            answer("111 63 9", MIC_FIRST),
            answer("111 63 9", MIC_LATER),
            answer("63 111 9", MIC_LATER),
            answer("111 9", "minptime=10;useinbandfec=1"),
        ] {
            assert_eq!(pin(&offer, &server_answer, &["mic"], &mut pins), server_answer);
        }
        assert!(pins.is_empty());
    }

    #[test]
    fn an_external_tracks_section_keeps_its_first_answer_through_a_dtx_change() {
        let offer = screen_audio_offer();
        let mut pins = HashMap::new();
        let first = answer("111 63 9", MIC_FIRST);
        assert_eq!(pin(&offer, &first, &[], &mut pins), first);
        for later in [answer("111 63 9", MIC_LATER), answer("63 111 9", MIC_LATER)] {
            assert_eq!(pin(&offer, &later, &[], &mut pins), first);
        }
        assert_eq!(pins.keys().collect::<Vec<_>>(), ["1"]);
    }

    #[test]
    fn one_answer_pins_the_external_section_and_passes_the_device_section() {
        let offer = audio_sdp(&[
            ("0", "recvonly", None, "111 63 9", OFFERED),
            ("1", "sendonly", Some("mic"), "111 63 9", OFFERED),
            ("2", "sendonly", Some("screen-audio"), "111 63 9", OFFERED),
        ]);
        let mixed = |mic_formats: &str, mic_fmtp: &str, screen_formats: &str, screen_fmtp: &str| {
            audio_sdp(&[
                ("0", "sendonly", None, "111 63 9", "minptime=10;useinbandfec=1"),
                ("1", "recvonly", None, mic_formats, mic_fmtp),
                ("2", "recvonly", None, screen_formats, screen_fmtp),
            ])
        };
        let mut pins = HashMap::new();
        let first = mixed("111 63 9", MIC_FIRST, "111 63 9", "minptime=10;useinbandfec=1");
        assert_eq!(
            pin(&offer, &first, &["screen-audio"], &mut pins),
            mixed("111 63 9", MIC_FIRST, "111 63 9", UNRESOLVED)
        );
        assert_eq!(pins.keys().collect::<Vec<_>>(), ["2"]);

        let later = mixed("63 111 9", MIC_LATER, "63 111 9", RESOLVED);
        assert_eq!(
            pin(&offer, &later, &["screen-audio"], &mut pins),
            mixed("63 111 9", MIC_LATER, "111 63 9", UNRESOLVED)
        );
        let mic_only = mixed("111 63 9", MIC_LATER, "111 63 9", UNRESOLVED);
        assert_eq!(pin(&offer, &mic_only, &["screen-audio"], &mut pins), mic_only);
    }

    /// A real publisher keeps its screen-audio send codec when a later answer changes the Opus
    /// parameters, as LiveKit's do once the server resolves the track.
    #[tokio::test]
    async fn an_external_tracks_send_codec_survives_an_answer_that_changes_opus_parameters() {
        send_codec_across_a_changed_answer(true).await;
    }

    /// The same track left unregistered stands in for a device track (those need the platform
    /// ADM): the transport passes the changed answer on and the send codec follows it.
    #[tokio::test]
    async fn an_unregistered_tracks_send_codec_follows_an_answer_that_changes_opus_parameters() {
        send_codec_across_a_changed_answer(false).await;
    }

    async fn send_codec_across_a_changed_answer(external: bool) {
        use std::{
            sync::{Arc, Mutex},
            time::{Duration, Instant},
        };

        use libwebrtc::{
            audio_source::native::NativeAudioSource,
            peer_connection_factory::native::PeerConnectionFactoryExt, prelude::*, stats::RtcStats,
        };
        use livekit_protocol as proto;

        async fn audio_send_codec(pc: &PeerConnection) -> Option<(String, u32)> {
            let stats = pc.get_stats().await.unwrap();
            let codec_id = stats.iter().find_map(|stats| match stats {
                RtcStats::OutboundRtp(o) if o.stream.kind == "audio" => {
                    Some(o.stream.codec_id.clone())
                }
                _ => None,
            })?;
            stats.into_iter().find_map(|stats| match stats {
                RtcStats::Codec(c) if c.rtc.id == codec_id => {
                    Some((c.codec.sdp_fmtp_line, c.codec.channels))
                }
                _ => None,
            })
        }

        async fn negotiate(
            transport: &PeerTransport,
            offers: &Mutex<Vec<SessionDescription>>,
            bob: &PeerConnection,
            server: impl Fn(String) -> String,
            pinned: bool,
        ) {
            transport.create_and_send_offer(OfferOptions::default()).await.unwrap();
            let offer = offers.lock().unwrap().pop().unwrap();
            bob.set_remote_description(offer).await.unwrap();
            let answer = bob.create_answer(AnswerOptions::default()).await.unwrap();
            bob.set_local_description(answer.clone()).await.unwrap();
            let answer =
                SessionDescription::parse(&server(answer.to_string()), SdpType::Answer).unwrap();
            if pinned {
                transport.set_remote_description(answer).await.unwrap();
            } else {
                transport.peer_connection().set_remote_description(answer).await.unwrap();
            }
        }

        async fn settled_codec(
            alice: &PeerConnection,
            previous: Option<&(String, u32)>,
        ) -> (String, u32) {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                tokio::time::sleep(Duration::from_millis(100)).await;
                if let Some(codec) = audio_send_codec(alice).await {
                    if previous.is_none_or(|p| *p != codec) || Instant::now() > deadline {
                        return codec;
                    }
                }
                assert!(Instant::now() < deadline, "the audio sender never reported a codec");
            }
        }

        let factory = PeerConnectionFactory::default();
        let alice = factory.create_peer_connection(RtcConfiguration::default()).unwrap();
        let bob = factory.create_peer_connection(RtcConfiguration::default()).unwrap();
        for (from, to) in [(alice.clone(), bob.clone()), (bob.clone(), alice.clone())] {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<IceCandidate>();
            from.on_ice_candidate(Some(Box::new(move |candidate| {
                let _ = tx.send(candidate);
            })));
            tokio::spawn(async move {
                while let Some(candidate) = rx.recv().await {
                    let _ = to.add_ice_candidate(candidate).await;
                }
            });
        }

        let transport = PeerTransport::new(alice.clone(), proto::SignalTarget::Publisher, false);
        let offers = Arc::new(Mutex::new(Vec::new()));
        let emitted = offers.clone();
        transport.on_offer(Some(Box::new(move |offer| emitted.lock().unwrap().push(offer))));

        let source = NativeAudioSource::new(AudioSourceOptions::default(), 48_000, 2, 100);
        let track = factory.create_audio_track("screen-audio", source);
        transport.add_stereo_track("screen-audio".to_string()).await;
        if external {
            transport.add_external_audio_track("screen-audio".to_string()).await;
        }
        alice
            .add_transceiver(
                MediaStreamTrack::Audio(track),
                RtpTransceiverInit {
                    direction: RtpTransceiverDirection::SendOnly,
                    stream_ids: Vec::new(),
                    send_encodings: Vec::new(),
                },
            )
            .unwrap();

        let resolved =
            |sdp: String| sdp.replace("a=fmtp:111 ", "a=fmtp:111 maxaveragebitrate=510000;");
        negotiate(&transport, &offers, &bob, |sdp| sdp, true).await;
        let first = settled_codec(&alice, None).await;
        assert_eq!(first.1, 2, "{first:?}");
        assert!(!first.0.contains("maxaveragebitrate"), "{first:?}");

        negotiate(&transport, &offers, &bob, resolved, true).await;
        if external {
            assert!(first.0.contains("stereo=1"), "{first:?}");
            tokio::time::sleep(Duration::from_millis(300)).await;
            assert_eq!(audio_send_codec(&alice).await.unwrap(), first);
            let remote = alice.current_remote_description().unwrap().to_string();
            assert!(!remote.contains("maxaveragebitrate"), "{remote}");

            // The same answer without the pin does change the send codec, so the check above can
            // fail.
            negotiate(&transport, &offers, &bob, resolved, false).await;
            let unpinned = settled_codec(&alice, Some(&first)).await;
            assert!(unpinned.0.contains("maxaveragebitrate=510000"), "{unpinned:?}");
        } else {
            let followed = settled_codec(&alice, Some(&first)).await;
            assert!(followed.0.contains("maxaveragebitrate=510000"), "{followed:?}");
        }

        alice.close();
        bob.close();
    }
}
