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
        Some(BitrateSettings { max_bitrate_bps: Some(self.max_bitrate_bps()), ..Default::default() })
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
            })),
            bitrate: Mutex::new(BitratePreferences::default()),
        }
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
            self.peer_connection.set_local_description(pending_offer).await?;
        }

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
                    Ok(parsed) => offer = parsed,
                    Err(e) => {
                        log::warn!("Failed to parse stereo-track-munged SDP, using original: {e}");
                    }
                }
            }
        }

        self.peer_connection.set_local_description(offer.clone()).await?;

        if let Some(handler) = self.on_offer_handler.lock().as_mut() {
            handler(offer);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use libwebrtc::peer_connection::BitrateSettings;

    use super::{BitratePreferences, PeerTransport};

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
        BitrateSettings { start_bitrate_bps: start, max_bitrate_bps: Some(max), ..Default::default() }
    }

    #[test]
    fn only_the_first_video_track_applies_a_start_bitrate() {
        let mut prefs = BitratePreferences::default();
        assert_eq!(prefs.on_video_track("low".into(), Some(200_000), false), settings(None, 5_000_000));
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
            let source = NativeVideoSource::new(VideoResolution { width: 1280, height: 720 }, screen);
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
}
