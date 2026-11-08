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
    fmt::{Debug, Display},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use libwebrtc::{
    native::packet_trailer::{
        self, PacketTrailerHandler, PublishTimingObserver as RtcPublishTimingObserver,
    },
    prelude::*,
    stats::RtcStats,
};
use livekit_protocol as proto;
use parking_lot::Mutex;
use tokio::sync::broadcast;
use tokio_stream::{wrappers::BroadcastStream, Stream};

use super::TrackInner;
use crate::{prelude::*, rtc_engine::lk_runtime::LkRuntime};

pub use libwebrtc::native::packet_trailer::{PublishTimingEvent, PublishTimingStage};
pub use libwebrtc::video_track::VideoContentHint;

const PUBLISH_TIMING_BUFFER: usize = 256;

/// Encoder settings to change on a published video track, applied with
/// [`LocalVideoTrack::set_sender_encoding`]. `None` leaves a setting as it is.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SenderEncodingUpdate {
    /// Bitrate cap of the top layer, in bits per second.
    pub max_bitrate_bps: Option<u64>,
    /// Framerate cap of the top layer. Lower layers are held at or below it.
    pub max_framerate: Option<f64>,
    pub degradation_preference: Option<DegradationPreference>,
    pub content_hint: Option<VideoContentHint>,
}

impl SenderEncodingUpdate {
    fn validate(&self) -> RoomResult<()> {
        if self.max_bitrate_bps == Some(0) {
            return Err(RoomError::Internal("max_bitrate_bps must be above 0".into()));
        }
        if let Some(fps) = self.max_framerate {
            if !fps.is_finite() || fps <= 0.0 {
                return Err(RoomError::Internal(format!(
                    "max_framerate must be a positive number, got {fps}"
                )));
            }
        }
        Ok(())
    }

    fn changes_parameters(&self) -> bool {
        self.max_bitrate_bps.is_some()
            || self.max_framerate.is_some()
            || self.degradation_preference.is_some()
    }
}

/// A simulcast layer currently configured on a local video publisher.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishingLayer {
    /// RTP stream identifier for this simulcast layer.
    pub rid: String,
    /// Video quality represented by this simulcast layer.
    pub quality: PublishingLayerQuality,
    /// Whether this simulcast layer is currently being published.
    pub active: bool,
}

/// Video quality for a publishing layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublishingLayerQuality {
    /// Low video quality.
    Low,
    /// Medium video quality.
    Medium,
    /// High video quality.
    High,
    /// The layer is disabled and not being encoded/sent.
    ///
    /// Mirrors `VideoQuality::OFF` from the LiveKit protocol. In practice this
    /// is not emitted by [`LocalVideoTrack::publishing_layers`] (a disabled
    /// layer is instead reflected by [`PublishingLayer::active`] being
    /// `false`); it exists to fully model the protocol enum and for conversions
    /// to/from it.
    Off,
}

impl From<proto::VideoQuality> for PublishingLayerQuality {
    fn from(quality: proto::VideoQuality) -> Self {
        match quality {
            proto::VideoQuality::Low => Self::Low,
            proto::VideoQuality::Medium => Self::Medium,
            proto::VideoQuality::High => Self::High,
            proto::VideoQuality::Off => Self::Off,
        }
    }
}

impl From<PublishingLayerQuality> for proto::VideoQuality {
    fn from(quality: PublishingLayerQuality) -> Self {
        match quality {
            PublishingLayerQuality::Low => Self::Low,
            PublishingLayerQuality::Medium => Self::Medium,
            PublishingLayerQuality::High => Self::High,
            PublishingLayerQuality::Off => Self::Off,
        }
    }
}

impl Display for PublishingLayerQuality {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Low => write!(f, "low"),
            Self::Medium => write!(f, "medium"),
            Self::High => write!(f, "high"),
            Self::Off => write!(f, "off"),
        }
    }
}

#[derive(Clone)]
pub struct LocalVideoTrack {
    inner: Arc<TrackInner>,
    source: RtcVideoSource,
    packet_trailer_handler: Arc<Mutex<Option<PacketTrailerHandler>>>,
    publish_timing_tx: Arc<Mutex<Option<broadcast::Sender<PublishTimingEvent>>>>,
    /// Last qualities requested by dynacast, re-applied after renegotiation.
    subscribed_qualities: Arc<Mutex<Option<Vec<proto::SubscribedQuality>>>>,
    /// Serializes read-modify-write of the sender's parameters (dynacast, encoding updates).
    sender_params: Arc<Mutex<SenderParamsState>>,
}

#[derive(Default)]
struct SenderParamsState {
    /// Each layer's framerate cap before the first encoding update on the current sender.
    framerate_baseline: Option<Vec<(String, Option<f64>)>>,
}

impl Debug for LocalVideoTrack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalVideoTrack")
            .field("sid", &self.sid())
            .field("name", &self.name())
            .field("source", &self.source())
            .finish()
    }
}

/// A stream of native local video publish-pipeline timing events.
pub struct PublishTimingEventStream {
    inner: BroadcastStream<PublishTimingEvent>,
}

impl Stream for PublishTimingEventStream {
    type Item = PublishTimingEvent;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            match Pin::new(&mut this.inner).poll_next(cx) {
                Poll::Ready(Some(Ok(event))) => return Poll::Ready(Some(event)),
                Poll::Ready(Some(Err(_))) => continue,
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

impl LocalVideoTrack {
    pub fn new(name: String, rtc_track: RtcVideoTrack, source: RtcVideoSource) -> Self {
        Self {
            inner: Arc::new(super::new_inner(
                "TR_unknown".to_owned().try_into().unwrap(),
                name,
                TrackKind::Video,
                MediaStreamTrack::Video(rtc_track),
            )),
            source,
            packet_trailer_handler: Arc::new(Mutex::new(None)),
            publish_timing_tx: Arc::new(Mutex::new(None)),
            subscribed_qualities: Arc::new(Mutex::new(None)),
            sender_params: Arc::new(Mutex::new(SenderParamsState::default())),
        }
    }

    pub fn create_video_track(name: &str, source: RtcVideoSource) -> LocalVideoTrack {
        let rtc_track = match source.clone() {
            #[cfg(not(target_arch = "wasm32"))]
            RtcVideoSource::Native(native_source) => {
                use libwebrtc::peer_connection_factory::native::PeerConnectionFactoryExt;
                LkRuntime::instance()
                    .pc_factory()
                    .create_video_track(&libwebrtc::native::create_random_uuid(), native_source)
            }
            _ => panic!("unsupported video source"),
        };

        Self::new(name.to_string(), rtc_track, source)
    }

    pub fn sid(&self) -> TrackSid {
        self.inner.info.read().sid.clone()
    }

    pub fn name(&self) -> String {
        self.inner.info.read().name.clone()
    }

    pub fn kind(&self) -> TrackKind {
        self.inner.info.read().kind
    }

    pub fn source(&self) -> TrackSource {
        self.inner.info.read().source
    }

    pub fn stream_state(&self) -> StreamState {
        self.inner.info.read().stream_state
    }

    pub fn is_enabled(&self) -> bool {
        self.inner.rtc_track.enabled()
    }

    pub fn enable(&self) {
        self.inner.rtc_track.set_enabled(true);
    }

    pub fn disable(&self) {
        self.inner.rtc_track.set_enabled(false);
    }

    pub fn is_muted(&self) -> bool {
        self.inner.info.read().muted
    }

    pub fn mute(&self) {
        super::set_muted(&self.inner, &Track::LocalVideo(self.clone()), true);
    }

    pub fn unmute(&self) {
        super::set_muted(&self.inner, &Track::LocalVideo(self.clone()), false);
    }

    pub fn rtc_track(&self) -> RtcVideoTrack {
        if let MediaStreamTrack::Video(video) = self.inner.rtc_track.clone() {
            return video;
        }
        unreachable!();
    }

    pub fn is_remote(&self) -> bool {
        false
    }

    pub fn rtc_source(&self) -> RtcVideoSource {
        self.source.clone()
    }

    /// Returns a stream of native local video publish-pipeline timing events.
    ///
    /// Multiple concurrent subscriptions are supported; each call returns an
    /// independent stream that begins with the next event observed after this
    /// call. Slow consumers will silently drop intermediate events when the
    /// internal buffer fills.
    ///
    /// The underlying transformer is allocated lazily on first call. If invoked
    /// before the track is published, instrumentation is enabled at publish time.
    pub fn publish_timing_events(&self) -> PublishTimingEventStream {
        let tx = {
            let mut publish_timing_tx = self.publish_timing_tx.lock();
            if let Some(tx) = publish_timing_tx.as_ref() {
                tx.clone()
            } else {
                let (tx, _) = broadcast::channel(PUBLISH_TIMING_BUFFER);
                *publish_timing_tx = Some(tx.clone());
                tx
            }
        };

        let handler = self.ensure_publish_timing_handler();
        if let Some(handler) = handler {
            self.apply_publish_timing_observer(&handler);
        }

        PublishTimingEventStream { inner: BroadcastStream::new(tx.subscribe()) }
    }

    /// Returns the packet trailer handler associated with this track, if any.
    /// When present on the sender side, callers can store per-frame user
    /// timestamps which will be embedded into encoded frames.
    pub(crate) fn packet_trailer_handler(&self) -> Option<PacketTrailerHandler> {
        self.packet_trailer_handler.lock().clone()
    }

    pub(crate) fn has_publish_timing_subscribers(&self) -> bool {
        self.publish_timing_tx.lock().is_some()
    }

    /// Internal: set the packet trailer handler used for this track.
    pub(crate) fn set_packet_trailer_handler(&self, handler: PacketTrailerHandler) {
        self.apply_publish_timing_observer(&handler);
        *self.packet_trailer_handler.lock() = Some(handler);
    }

    fn ensure_publish_timing_handler(&self) -> Option<PacketTrailerHandler> {
        if let Some(handler) = self.packet_trailer_handler.lock().clone() {
            return Some(handler);
        }

        let transceiver = self.transceiver()?;
        let handler = packet_trailer::create_sender_handler(
            LkRuntime::instance().pc_factory(),
            &transceiver.sender(),
        );
        handler.set_enabled(false);
        self.set_packet_trailer_handler(handler.clone());

        #[cfg(not(target_arch = "wasm32"))]
        if let RtcVideoSource::Native(ref native_source) = self.rtc_source() {
            native_source.set_packet_trailer_handler(handler.clone());
        }

        Some(handler)
    }

    fn apply_publish_timing_observer(&self, handler: &PacketTrailerHandler) {
        let tx = self.publish_timing_tx.lock().clone();
        let observer = tx.map(|tx| {
            Arc::new(move |event: PublishTimingEvent| {
                let _ = tx.send(event);
            }) as RtcPublishTimingObserver
        });
        handler.set_publish_timing_observer(observer);
    }

    pub async fn get_stats(&self) -> RoomResult<Vec<RtcStats>> {
        super::local_track::get_stats(&self.inner).await
    }

    pub(crate) fn on_muted(&self, f: impl Fn(Track) + Send + 'static) {
        self.inner.events.lock().muted.replace(Box::new(f));
    }

    pub(crate) fn on_unmuted(&self, f: impl Fn(Track) + Send + 'static) {
        self.inner.events.lock().unmuted.replace(Box::new(f));
    }

    pub(crate) fn transceiver(&self) -> Option<RtpTransceiver> {
        self.inner.info.read().transceiver.clone()
    }

    pub(crate) fn set_transceiver(&self, transceiver: Option<RtpTransceiver>) {
        // clear subscribed qualities on unpublish
        if transceiver.is_none() {
            *self.subscribed_qualities.lock() = None;
        }
        self.sender_params.lock().framerate_baseline = None;
        self.inner.info.write().transceiver = transceiver;
    }

    pub(crate) fn update_info(&self, info: proto::TrackInfo) {
        super::update_info(&self.inner, &Track::LocalVideo(self.clone()), info);
    }

    /// Returns a snapshot of each simulcast layer's RID, quality, and active state.
    /// Useful for diagnostics / HUD display.
    /// Returns an empty vec if no transceiver is set, or if the sender has no encodings yet.
    pub fn publishing_layers(&self) -> Vec<PublishingLayer> {
        let Some(transceiver) = self.transceiver() else {
            log::debug!("dynacast: no transceiver, returning empty layers");
            return Vec::new();
        };
        let params = transceiver.sender().parameters();
        params
            .encodings
            .iter()
            .map(|e| {
                let quality = crate::options::video_quality_for_rid_or_default(&e.rid);
                PublishingLayer { rid: e.rid.clone(), quality: quality.into(), active: e.active }
            })
            .collect()
    }

    /// Toggle simulcast encoding layers on/off based on subscriber demand.
    /// Used by dynacast: the SFU tells us which quality levels are needed,
    /// and we set `encoding.active` accordingly on the RTP sender.
    pub(crate) fn set_publishing_layers(
        &self,
        qualities: &[proto::SubscribedQuality],
    ) -> RoomResult<()> {
        merge_qualities(self.subscribed_qualities.lock().get_or_insert_with(Vec::new), qualities);
        self.apply_publishing_layers(qualities)
    }

    /// Re-applies the last dynacast qualities, since renegotiation re-enables paused layers.
    pub(crate) fn refresh_publishing_layers(&self) -> RoomResult<()> {
        let Some(qualities) = self.subscribed_qualities.lock().clone() else {
            return Ok(());
        };
        self.apply_publishing_layers(&qualities)
    }

    fn apply_publishing_layers(&self, qualities: &[proto::SubscribedQuality]) -> RoomResult<()> {
        let transceiver = self.transceiver().ok_or_else(|| {
            RoomError::Internal("cannot set publishing layers: no transceiver".into())
        })?;

        let sender = transceiver.sender();
        let _sender_params = self.sender_params.lock();
        let mut params = sender.parameters();

        if params.encodings.is_empty() {
            log::debug!("dynacast: no sender encodings available, ignoring quality update");
            return Ok(());
        }

        let mut changed = false;
        for encoding in &mut params.encodings {
            // The SFU addresses layers by spatial index (0 = Low), so a
            // single rid-less encoding is addressed as Low, not High.
            let rid = if encoding.rid.is_empty() { "q" } else { encoding.rid.as_str() };
            let quality = crate::options::video_quality_for_rid_or_default(rid);

            // A quality missing from the update is left untouched.
            let Some(subscribed) = qualities.iter().find(|q| q.quality == quality as i32) else {
                continue;
            };

            if encoding.active != subscribed.enabled {
                changed = true;
                encoding.active = subscribed.enabled;
            }
        }

        let layers: Vec<String> = params
            .encodings
            .iter()
            .map(|e| {
                let quality = crate::options::video_quality_for_rid_or_default(&e.rid);
                let state = if e.active { "ON" } else { "off" };
                format!("{}({:?})={}", e.rid, quality, state)
            })
            .collect();

        if changed {
            sender.set_parameters(params).map_err(|e| {
                RoomError::Internal(format!("failed to set sender parameters: {}", e))
            })?;
            log::debug!("dynacast: layers changed -> [{}]", layers.join(", "));
        } else {
            log::debug!("dynacast: layers unchanged [{}]", layers.join(", "));
        }

        Ok(())
    }

    /// Changes the encoder settings of the published track in place, through the sender's
    /// `setParameters`: no renegotiation, and the track keeps its sid.
    ///
    /// The bitrate and framerate caps go to the top layer. A framerate cap also bounds every
    /// other layer, which keeps the lower of the cap and the framerate it had before the first
    /// update. Layer `active` flags stay with dynacast. The content hint is set on the track, and
    /// alone it needs no sender. A republish starts over from the publish options.
    pub fn set_sender_encoding(&self, update: &SenderEncodingUpdate) -> RoomResult<()> {
        update.validate()?;

        if update.changes_parameters() {
            let transceiver = self.transceiver().ok_or_else(|| {
                RoomError::Internal("cannot set the sender encoding: track not published".into())
            })?;
            let sender = transceiver.sender();
            let mut state = self.sender_params.lock();
            let mut params = sender.parameters();
            if params.encodings.is_empty()
                && (update.max_bitrate_bps.is_some() || update.max_framerate.is_some())
            {
                return Err(RoomError::Internal(
                    "cannot set the sender encoding: sender has no encodings".into(),
                ));
            }

            let baseline = state.framerate_baseline.get_or_insert_with(|| {
                params.encodings.iter().map(|e| (e.rid.clone(), e.max_framerate)).collect()
            });
            merge_sender_encodings(
                &mut params.encodings,
                baseline,
                update.max_bitrate_bps,
                update.max_framerate,
            );
            if let Some(preference) = update.degradation_preference {
                params.set_degradation_preference(preference);
            }

            sender.set_parameters(params).map_err(|e| {
                RoomError::Internal(format!("failed to set sender parameters: {}", e))
            })?;
            log::debug!("sender encoding updated: {:?}", update);
        }

        if let Some(hint) = update.content_hint {
            self.rtc_track().set_content_hint(hint);
        }

        Ok(())
    }

    /// Applies publishing-layer quality state for end-to-end tests.
    #[doc(hidden)]
    #[cfg(feature = "__lk-e2e-test")]
    pub fn set_publishing_layers_for_test(
        &self,
        qualities: &[(PublishingLayerQuality, bool)],
    ) -> RoomResult<()> {
        let qualities: Vec<proto::SubscribedQuality> = qualities
            .iter()
            .map(|(quality, enabled)| proto::SubscribedQuality {
                quality: proto::VideoQuality::from(*quality) as i32,
                enabled: *enabled,
            })
            .collect();
        self.set_publishing_layers(&qualities)
    }
}

/// The full-resolution layer: the smallest `scale_resolution_down_by`, then the highest RID.
fn top_layer_index(encodings: &[RtpEncodingParameters]) -> Option<usize> {
    let rid_rank =
        |rid: &str| crate::options::video_quality_for_rid(rid).map_or(-1, |quality| quality as i32);
    encodings
        .iter()
        .enumerate()
        .max_by(|(a_index, a), (b_index, b)| {
            let a_scale = a.scale_resolution_down_by.unwrap_or(1.0);
            let b_scale = b.scale_resolution_down_by.unwrap_or(1.0);
            b_scale
                .total_cmp(&a_scale)
                .then(rid_rank(&a.rid).cmp(&rid_rank(&b.rid)))
                .then(a_index.cmp(b_index))
        })
        .map(|(index, _)| index)
}

/// Puts the caps on the top layer and holds every other layer's framerate at the lower of the cap
/// and its `baseline` framerate (by RID, else its current one). Leaves `active` alone.
fn merge_sender_encodings(
    encodings: &mut [RtpEncodingParameters],
    baseline: &[(String, Option<f64>)],
    max_bitrate_bps: Option<u64>,
    max_framerate: Option<f64>,
) {
    let Some(top) = top_layer_index(encodings) else {
        return;
    };

    for (index, encoding) in encodings.iter_mut().enumerate() {
        if index == top {
            if let Some(bitrate) = max_bitrate_bps {
                encoding.max_bitrate = Some(bitrate);
            }
            if let Some(fps) = max_framerate {
                encoding.max_framerate = Some(fps);
            }
            continue;
        }

        let Some(cap) = max_framerate else {
            continue;
        };
        let own = baseline
            .iter()
            .find(|(rid, _)| *rid == encoding.rid)
            .map_or(encoding.max_framerate, |(_, fps)| *fps);
        encoding.max_framerate = Some(own.map_or(cap, |fps| fps.min(cap)));
    }
}

/// Merges `update` into `cached` by quality, leaving qualities missing from an update untouched.
fn merge_qualities(
    cached: &mut Vec<proto::SubscribedQuality>,
    update: &[proto::SubscribedQuality],
) {
    for quality in update {
        match cached.iter_mut().find(|q| q.quality == quality.quality) {
            Some(existing) => existing.enabled = quality.enabled,
            None => cached.push(*quality),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quality(quality: proto::VideoQuality, enabled: bool) -> proto::SubscribedQuality {
        proto::SubscribedQuality { quality: quality as i32, enabled }
    }

    #[test]
    fn merge_qualities_keeps_earlier_layers() {
        let mut cached = Vec::new();
        merge_qualities(&mut cached, &[quality(proto::VideoQuality::Medium, false)]);
        merge_qualities(&mut cached, &[quality(proto::VideoQuality::High, false)]);
        merge_qualities(&mut cached, &[quality(proto::VideoQuality::Medium, true)]);
        assert_eq!(
            cached,
            [quality(proto::VideoQuality::Medium, true), quality(proto::VideoQuality::High, false)]
        );
    }

    fn layer(rid: &str, scale: f64, bitrate: u64, fps: Option<f64>) -> RtpEncodingParameters {
        RtpEncodingParameters {
            rid: rid.to_owned(),
            scale_resolution_down_by: Some(scale),
            max_bitrate: Some(bitrate),
            max_framerate: fps,
            ..Default::default()
        }
    }

    fn simulcast() -> Vec<RtpEncodingParameters> {
        vec![
            layer("q", 4.0, 150_000, Some(15.0)),
            layer("h", 2.0, 500_000, Some(30.0)),
            layer("f", 1.0, 2_500_000, Some(30.0)),
        ]
    }

    fn baseline(encodings: &[RtpEncodingParameters]) -> Vec<(String, Option<f64>)> {
        encodings.iter().map(|e| (e.rid.clone(), e.max_framerate)).collect()
    }

    fn caps(encodings: &[RtpEncodingParameters]) -> Vec<(Option<u64>, Option<f64>)> {
        encodings.iter().map(|e| (e.max_bitrate, e.max_framerate)).collect()
    }

    #[test]
    fn single_encoding_takes_both_caps() {
        let mut encodings = vec![RtpEncodingParameters {
            max_bitrate: Some(1_000_000),
            max_framerate: Some(30.0),
            ..Default::default()
        }];
        let base = baseline(&encodings);
        merge_sender_encodings(&mut encodings, &base, Some(6_000_000), Some(60.0));
        assert_eq!(caps(&encodings), [(Some(6_000_000), Some(60.0))]);
    }

    #[test]
    fn bitrate_goes_to_the_top_layer_only() {
        let mut encodings = simulcast();
        let base = baseline(&encodings);
        merge_sender_encodings(&mut encodings, &base, Some(4_000_000), None);
        assert_eq!(
            caps(&encodings),
            [
                (Some(150_000), Some(15.0)),
                (Some(500_000), Some(30.0)),
                (Some(4_000_000), Some(30.0))
            ]
        );
    }

    #[test]
    fn framerate_cap_bounds_lower_layers_that_exceed_it() {
        let mut encodings = simulcast();
        let base = baseline(&encodings);
        merge_sender_encodings(&mut encodings, &base, None, Some(20.0));
        assert_eq!(
            caps(&encodings),
            [
                (Some(150_000), Some(15.0)),
                (Some(500_000), Some(20.0)),
                (Some(2_500_000), Some(20.0))
            ]
        );
    }

    #[test]
    fn raising_the_cap_restores_lower_layers_to_their_baseline() {
        let mut encodings = simulcast();
        let base = baseline(&encodings);
        merge_sender_encodings(&mut encodings, &base, None, Some(5.0));
        assert_eq!(caps(&encodings)[0].1, Some(5.0));
        assert_eq!(caps(&encodings)[1].1, Some(5.0));

        merge_sender_encodings(&mut encodings, &base, None, Some(60.0));
        assert_eq!(
            caps(&encodings),
            [
                (Some(150_000), Some(15.0)),
                (Some(500_000), Some(30.0)),
                (Some(2_500_000), Some(60.0))
            ]
        );
    }

    #[test]
    fn uncapped_lower_layer_takes_the_cap() {
        let mut encodings = simulcast();
        encodings[0].max_framerate = None;
        let base = baseline(&encodings);
        merge_sender_encodings(&mut encodings, &base, None, Some(24.0));
        assert_eq!(caps(&encodings)[0].1, Some(24.0));
    }

    #[test]
    fn active_flags_are_left_alone() {
        let mut encodings = simulcast();
        encodings[1].active = false;
        encodings[2].active = false;
        let base = baseline(&encodings);
        merge_sender_encodings(&mut encodings, &base, Some(3_000_000), Some(10.0));
        let active: Vec<bool> = encodings.iter().map(|e| e.active).collect();
        assert_eq!(active, [true, false, false]);
        assert_eq!(encodings[2].max_bitrate, Some(3_000_000));
    }

    #[test]
    fn top_layer_is_found_by_scale_not_order() {
        let mut encodings = simulcast();
        encodings.reverse();
        let base = baseline(&encodings);
        merge_sender_encodings(&mut encodings, &base, Some(8_000_000), None);
        assert_eq!(encodings[0].rid, "f");
        assert_eq!(encodings[0].max_bitrate, Some(8_000_000));
        assert_eq!(encodings[2].max_bitrate, Some(150_000));
    }

    #[test]
    fn top_layer_falls_back_to_rid_when_scales_tie() {
        let mut encodings = simulcast();
        for encoding in &mut encodings {
            encoding.scale_resolution_down_by = None;
        }
        assert_eq!(top_layer_index(&encodings), Some(2));
        encodings.swap(0, 2);
        assert_eq!(top_layer_index(&encodings), Some(0));
    }

    #[test]
    fn no_caps_change_nothing() {
        let mut encodings = simulcast();
        let base = baseline(&encodings);
        merge_sender_encodings(&mut encodings, &base, None, None);
        assert_eq!(caps(&encodings), caps(&simulcast()));
        merge_sender_encodings(&mut [], &[], Some(1), Some(1.0));
    }

    #[test]
    fn update_validation() {
        let ok = SenderEncodingUpdate {
            max_bitrate_bps: Some(1),
            max_framerate: Some(0.5),
            ..Default::default()
        };
        assert!(ok.validate().is_ok());
        let zero_bitrate = SenderEncodingUpdate { max_bitrate_bps: Some(0), ..Default::default() };
        assert!(zero_bitrate.validate().is_err());
        for fps in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let update = SenderEncodingUpdate { max_framerate: Some(fps), ..Default::default() };
            assert!(update.validate().is_err(), "{fps} accepted");
        }
        let hint_only = SenderEncodingUpdate {
            content_hint: Some(VideoContentHint::Text),
            ..Default::default()
        };
        assert!(!hint_only.changes_parameters());
    }
}
