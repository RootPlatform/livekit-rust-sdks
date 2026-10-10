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

//! RootApp native audio requests (rootapp_audio.proto, oneof cases from 1000).
//!
//! Device requests are queued on the native device thread before the handler
//! returns and complete with a `RootAudioEvent`; nothing here blocks the caller.
//! Controls, track gains and effects store atomics or take short locks the
//! audio threads never take.
//!
//! Track gains are keyed by the remote track's FFI handle and applied to the
//! playout mixer by SSRC: the receiver's signaled SSRC (the SFU signals one per
//! subscribed track), or the track's inbound-rtp SSRC for an unsignaled stream.
//! While a session is live, a remote audio track's SSRC is resolved as it is
//! subscribed, so the host's first gain for it applies inside the request;
//! otherwise it is resolved off the caller's thread. Unknown SSRCs are
//! resolved every second, known ones every 5 s and shortly after the mixer
//! gets a new stream (a renegotiated SSRC), so the gain moves along; gains of
//! dropped track handles are forgotten.

use std::{
    collections::{BTreeSet, HashMap},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, LazyLock,
    },
    time::Duration,
};

use livekit::{
    prelude::*, sanitize_volume, webrtc::stats::RtcStats, AudioDeviceResults, AudioDeviceSelection,
    CaptureFormat, DenoiserCompletion, DenoiserState, EffectPlay, LocalSpeaking, PlatformAudio,
    PushToTalk, ReturnedPipeline, RootAudioControl, RootAudioStats,
};
use parking_lot::Mutex;
use tokio::time::MissedTickBehavior;

use super::{
    platform_audio::{platform_audio_from_handle, FfiPlatformAudio},
    room::FfiTrack,
    FfiServer,
};
use crate::{proto, FfiHandleId, FfiResult};

pub const CAP_SELECT_DEVICES: u64 = 1 << 0;
pub const CAP_PLAYOUT_HOLD: u64 = 1 << 1;
pub const CAP_STATS: u64 = 1 << 2;
pub const CAP_ADM_HARDENED: u64 = 1 << 3;
pub const CAP_MIXER: u64 = 1 << 4;
pub const CAP_TRACK_GAINS: u64 = 1 << 5;
pub const CAP_EFFECTS: u64 = 1 << 6;
pub const CAP_CONTROL: u64 = 1 << 7;
pub const CAP_DSP: u64 = 1 << 8;
pub const CAP_DENOISER: u64 = 1 << 9;
pub const CAP_MONITOR: u64 = 1 << 10;
pub const CAP_APM_DECORATOR: u64 = 1 << 11;

/// What `livekit_ffi_rootapp_audio_caps` reports. The ADM hardening is desktop
/// only, so mobile builds report nothing.
pub const ROOTAPP_AUDIO_CAPS: u64 = if cfg!(any(target_os = "ios", target_os = "android")) {
    0
} else {
    CAP_SELECT_DEVICES
        | CAP_PLAYOUT_HOLD
        | CAP_STATS
        | CAP_ADM_HARDENED
        | CAP_MIXER
        | CAP_TRACK_GAINS
        | CAP_EFFECTS
        | CAP_CONTROL
        | CAP_APM_DECORATOR
};

const NOT_SUPPORTED: &str = "not supported by this build";

/// How often the session task reads the native format and source counters.
const EVENT_POLL: Duration = Duration::from_millis(10);
/// The first poll waits this long, so the host listens for the handle's
/// events before a capture_format event is sent.
const FIRST_POLL_DELAY: Duration = Duration::from_millis(100);
/// Every this many polls the session resolves unknown SSRCs (1 s).
const RESOLVE_EVERY_POLLS: u32 = 100;
/// Known SSRCs are read again every this many passes (5 s), and after the
/// mixer got a new stream.
const KNOWN_RESOLVE_EVERY_PASSES: u32 = 5;
/// A new stream's pass waits this many polls, so the remote description that
/// added it has updated the receivers' parameters too.
const NEW_SOURCE_PASS_DELAY_POLLS: u32 = 5;
/// The longest effect clip RegisterEffect accepts (60 s at 48 kHz).
const MAX_EFFECT_SAMPLES: u32 = 48_000 * 60;
/// The rates the DSP chain runs at.
const MIN_DENOISER_RATE: u32 = 8_000;
const MAX_DENOISER_RATE: u32 = 48_000;

/// What a session drives: the handle's PlatformAudio (a recorder in tests).
trait RootAudioSink {
    fn set_source_gain(&self, ssrc: u32, tile: f32, paused: bool) -> bool;
    fn clear_source_gain(&self, ssrc: u32);
    fn reset_controls(&self, close_gate: bool);
}

impl RootAudioSink for PlatformAudio {
    fn set_source_gain(&self, ssrc: u32, tile: f32, paused: bool) -> bool {
        PlatformAudio::set_source_gain(self, ssrc, tile, paused)
    }

    fn clear_source_gain(&self, ssrc: u32) {
        PlatformAudio::clear_source_gain(self, ssrc)
    }

    fn reset_controls(&self, close_gate: bool) {
        self.reset_rootapp_controls(close_gate)
    }
}

fn sink_of(audio: &PlatformAudio) -> &dyn RootAudioSink {
    audio
}

/// The live sessions of the process. `PlatformAudio::new` reuses the shared
/// engine, so every handle drives the same controls: they go back to their
/// defaults only when the first session starts, and the capture gate closes
/// (with every other control reset and every denoiser pipeline handed back)
/// only when the last one ends. Dropping one handle never changes another's
/// state, and a teardown never opens the gate.
#[derive(Default)]
struct Sessions {
    /// The live handles; the newest is the largest.
    live: BTreeSet<FfiHandleId>,
    /// SSRCs of remote audio tracks, resolved as they were subscribed.
    subscribed: HashMap<FfiHandleId, u32>,
    mail: DenoiserMail,
}

impl Sessions {
    fn open(&mut self, handle: FfiHandleId, sink: &dyn RootAudioSink) {
        self.live.insert(handle);
        if self.live.len() == 1 {
            sink.reset_controls(false);
        }
        self.mail.opened(handle);
    }

    fn close(&mut self, handle: FfiHandleId, sink: &dyn RootAudioSink) {
        if !self.live.remove(&handle) {
            return;
        }
        if self.live.is_empty() {
            sink.reset_controls(true);
            self.subscribed.clear();
        }
        self.mail.closed(&self.live, handle);
    }

    fn deliver(&mut self, completions: Vec<DenoiserCompletion>, returned: Vec<ReturnedPipeline>) {
        self.mail.deliver(&self.live, completions, returned);
    }
}

/// Where SetDenoiser completions and returned pipelines go, each exactly once:
/// a completion to the handle that asked (dropped once it is gone: native
/// never held an errored pipeline, and a held one comes back as returned), a
/// pipeline to the handle whose request installed it while that one lives,
/// else to the newest live handle, else to the next handle to open (and to a
/// ReturnPipelines response before that).
#[derive(Default)]
struct DenoiserMail {
    completions: HashMap<FfiHandleId, Vec<(u64, Option<String>)>>,
    returned: HashMap<FfiHandleId, Vec<u64>>,
    orphans: Vec<u64>,
}

impl DenoiserMail {
    fn deliver(
        &mut self,
        live: &BTreeSet<FfiHandleId>,
        completions: Vec<DenoiserCompletion>,
        returned: Vec<ReturnedPipeline>,
    ) {
        for completion in completions {
            if live.contains(&completion.owner) {
                self.completions
                    .entry(completion.owner)
                    .or_default()
                    .push((completion.async_id, completion.error));
            } else {
                log::debug!(
                    "RootAudio: denoiser result {} for dropped handle {}",
                    completion.async_id,
                    completion.owner
                );
            }
        }
        for returned in returned {
            self.return_to(live, returned.owner, returned.pipeline);
        }
    }

    fn return_to(&mut self, live: &BTreeSet<FfiHandleId>, owner: FfiHandleId, pipeline: u64) {
        let target = if live.contains(&owner) { Some(owner) } else { live.last().copied() };
        match target {
            Some(handle) => self.returned.entry(handle).or_default().push(pipeline),
            None => self.orphans.push(pipeline),
        }
    }

    fn opened(&mut self, handle: FfiHandleId) {
        if !self.orphans.is_empty() {
            let orphans = std::mem::take(&mut self.orphans);
            self.returned.entry(handle).or_default().extend(orphans);
        }
    }

    fn closed(&mut self, live: &BTreeSet<FfiHandleId>, handle: FfiHandleId) {
        self.completions.remove(&handle);
        for pipeline in self.returned.remove(&handle).unwrap_or_default() {
            self.return_to(live, handle, pipeline);
        }
    }

    /// The handle's completions (async id, error) and returned pipelines.
    fn take(&mut self, handle: FfiHandleId) -> (Vec<(u64, Option<String>)>, Vec<u64>) {
        (
            self.completions.remove(&handle).unwrap_or_default(),
            self.returned.remove(&handle).unwrap_or_default(),
        )
    }

    /// Every pipeline not sent yet, for a ReturnPipelines response.
    fn take_undelivered(&mut self) -> Vec<u64> {
        let mut pipelines = std::mem::take(&mut self.orphans);
        for (_, returned) in self.returned.drain() {
            pipelines.extend(returned);
        }
        pipelines
    }
}

static SESSIONS: LazyLock<Mutex<Sessions>> = LazyLock::new(Default::default);

struct TrackGain<T> {
    track: T,
    tile: f32,
    paused: bool,
    ssrc: Option<u32>,
    resolving: bool,
    /// Bumped by a resolve request that may see a newer SSRC than the
    /// resolution running; that resolution is then not applied but run again.
    generation: u64,
}

/// The tile gains of one session by remote track handle, and the SSRC each
/// track plays under.
struct TrackGains<T> {
    gains: HashMap<FfiHandleId, TrackGain<T>>,
}

impl<T> Default for TrackGains<T> {
    fn default() -> Self {
        Self { gains: HashMap::new() }
    }
}

impl<T: Clone> TrackGains<T> {
    /// Stores a gain (`paused` None keeps the track's) and applies it when the
    /// SSRC is known, from an earlier resolution or `subscribed_ssrc`. Returns
    /// whether the handle still has to be resolved.
    fn set(
        &mut self,
        sink: &dyn RootAudioSink,
        handle: FfiHandleId,
        track: T,
        tile: f32,
        paused: Option<bool>,
        subscribed_ssrc: Option<u32>,
    ) -> bool {
        let gain = self.gains.entry(handle).or_insert_with(|| TrackGain {
            track,
            tile: 1.0,
            paused: false,
            ssrc: None,
            resolving: false,
            generation: 0,
        });
        gain.tile = tile;
        if let Some(paused) = paused {
            gain.paused = paused;
        }
        if gain.ssrc.is_none() {
            gain.ssrc = subscribed_ssrc;
        }
        match gain.ssrc {
            Some(ssrc) => {
                apply_gain(sink, ssrc, gain.tile, gain.paused);
                false
            }
            None => true,
        }
    }

    /// The track to resolve and the generation to finish with, or None while
    /// one resolution of the handle runs (with `supersede`, that one then runs
    /// again instead of being applied).
    fn begin_resolve(&mut self, handle: FfiHandleId, supersede: bool) -> Option<(T, u64)> {
        let gain = self.gains.get_mut(&handle)?;
        if gain.resolving {
            if supersede {
                gain.generation = gain.generation.wrapping_add(1);
            }
            return None;
        }
        gain.resolving = true;
        Some((gain.track.clone(), gain.generation))
    }

    /// Ends a resolution. Its SSRC is applied (no sink: the session ended)
    /// unless a newer request came meanwhile; then true is returned and the
    /// handle is to be resolved again.
    fn finish_resolve(
        &mut self,
        sink: Option<&dyn RootAudioSink>,
        handle: FfiHandleId,
        generation: u64,
        ssrc: Option<u32>,
    ) -> bool {
        let Some(gain) = self.gains.get_mut(&handle) else {
            return false;
        };
        gain.resolving = false;
        if gain.generation != generation {
            return true;
        }
        let (Some(sink), Some(ssrc)) = (sink, ssrc) else {
            return false;
        };
        let previous = gain.ssrc.replace(ssrc);
        let (tile, paused) = (gain.tile, gain.paused);
        if previous != Some(ssrc) {
            if let Some(previous) = previous {
                self.clear_if_unused(sink, previous);
            }
            apply_gain(sink, ssrc, tile, paused);
        }
        false
    }

    /// Drops a handle's gain. Its SSRC's gain is forgotten once no other
    /// handle plays under it (a reused transceiver); the stream itself keeps
    /// its last gain.
    fn forget(&mut self, sink: Option<&dyn RootAudioSink>, handle: FfiHandleId) {
        let ssrc = self.gains.remove(&handle).and_then(|gain| gain.ssrc);
        if let (Some(sink), Some(ssrc)) = (sink, ssrc) {
            self.clear_if_unused(sink, ssrc);
        }
    }

    fn clear_if_unused(&self, sink: &dyn RootAudioSink, ssrc: u32) {
        if !self.gains.values().any(|gain| gain.ssrc == Some(ssrc)) {
            sink.clear_source_gain(ssrc);
        }
    }

    /// Every handle, and whether its SSRC is known.
    fn handles(&self) -> Vec<(FfiHandleId, bool)> {
        self.gains.iter().map(|(handle, gain)| (*handle, gain.ssrc.is_some())).collect()
    }

    fn clear(&mut self) {
        self.gains.clear();
    }
}

fn apply_gain(sink: &dyn RootAudioSink, ssrc: u32, tile: f32, paused: bool) {
    if !sink.set_source_gain(ssrc, tile, paused) {
        log::warn!("RootAudio: the mixer's gain table is full; ssrc {ssrc} has no gain");
    }
}

struct SessionState {
    /// None once the handle is dropped, so this session never keeps the ADM
    /// referenced past the host's dispose.
    audio: Option<PlatformAudio>,
    gains: TrackGains<RemoteAudioTrack>,
    resolve_passes: u32,
}

struct GainRequest {
    handle: FfiHandleId,
    track: RemoteAudioTrack,
    tile: f32,
    paused: Option<bool>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pass {
    Periodic,
    /// The mixer got a new stream: every SSRC is read again.
    NewSource,
}

/// What the events task reads from native on every poll.
struct NativePoll {
    format: CaptureFormat,
    sources: u64,
    speaking: LocalSpeaking,
    /// None without an Effects SDK slot.
    denoiser: Option<DenoiserState>,
    completions: Vec<DenoiserCompletion>,
    returned: Vec<ReturnedPipeline>,
}

impl NativePoll {
    fn read(audio: &PlatformAudio) -> Self {
        let denoiser = audio.denoiser_available();
        Self {
            format: audio.capture_format(),
            sources: audio.mixer_sources_added(),
            speaking: audio.speaking(),
            denoiser: denoiser.then(|| audio.denoiser_state()),
            completions: if denoiser { audio.take_denoiser_completions() } else { Vec::new() },
            returned: if denoiser { audio.take_returned_pipelines() } else { Vec::new() },
        }
    }
}

/// What one handle was last told: `speaking` is sent when it differs (so a
/// first poll sends it only when true), `denoiser_state` on the first poll and
/// on every change; denoiser results and returned pipelines as they arrive.
#[derive(Default)]
struct SessionEvents {
    speaking: bool,
    denoiser: Option<DenoiserState>,
}

impl SessionEvents {
    fn update(
        &mut self,
        handle: FfiHandleId,
        poll: NativePoll,
    ) -> Vec<proto::root_audio_event::Message> {
        let (completions, returned) = {
            let mut sessions = SESSIONS.lock();
            sessions.deliver(poll.completions, poll.returned);
            sessions.mail.take(handle)
        };
        self.messages(poll.speaking, poll.denoiser, completions, returned)
    }

    fn messages(
        &mut self,
        speaking: LocalSpeaking,
        denoiser: Option<DenoiserState>,
        completions: Vec<(u64, Option<String>)>,
        returned: Vec<u64>,
    ) -> Vec<proto::root_audio_event::Message> {
        let mut messages = Vec::new();
        for (async_id, error) in completions {
            messages.push(proto::RootAudioDenoiserResult { async_id, error }.into());
        }
        if !returned.is_empty() {
            messages.push(proto::RootAudioPipelinesReturned { pipelines: returned }.into());
        }
        if speaking.speaking != self.speaking {
            self.speaking = speaking.speaking;
            messages.push(
                proto::RootAudioSpeaking { speaking: speaking.speaking, seq: speaking.seq }.into(),
            );
        }
        if let Some(state) = denoiser.filter(|state| self.denoiser != Some(*state)) {
            self.denoiser = Some(state);
            messages.push(
                proto::RootAudioDenoiserState {
                    installed: state.installed,
                    rate: state.rate,
                    bypass_reason: state.bypass_reason,
                    webrtc_ns_fallback: state.webrtc_ns_fallback,
                }
                .into(),
            );
        }
        messages
    }
}

/// The FFI side of one `PlatformAudio` handle: the track gains it set and the
/// task that turns native state into `RootAudioEvent`s.
pub struct RootAudioSession {
    handle: FfiHandleId,
    state: Mutex<SessionState>,
    periodic_pass: AtomicBool,
}

impl RootAudioSession {
    /// Creates the session and starts its event task, which ends when the
    /// handle is dropped.
    pub fn start(
        server: &'static FfiServer,
        handle: FfiHandleId,
        audio: PlatformAudio,
    ) -> Arc<Self> {
        SESSIONS.lock().open(handle, &audio);
        let session = Arc::new(Self {
            handle,
            state: Mutex::new(SessionState {
                audio: Some(audio),
                gains: TrackGains::default(),
                resolve_passes: 0,
            }),
            periodic_pass: AtomicBool::new(false),
        });
        let dropped = server.watch_handle_dropped(handle);
        let task = server.async_runtime.spawn(session.clone().run(server, dropped));
        server.watch_panic(task);
        session
    }

    /// The handle was dropped. Runs before its PlatformAudio lets go of the
    /// device: the last live session closes the capture gate and resets the
    /// shared controls, any other leaves them to the sessions still live.
    /// Nothing resolved later is applied.
    pub fn end(&self) {
        let audio = {
            let mut state = self.state.lock();
            state.gains.clear();
            state.audio.take()
        };
        if let Some(audio) = audio {
            SESSIONS.lock().close(self.handle, &audio);
        }
    }

    async fn run(
        self: Arc<Self>,
        server: &'static FfiServer,
        mut dropped: tokio::sync::oneshot::Receiver<()>,
    ) {
        let mut interval =
            tokio::time::interval_at(tokio::time::Instant::now() + FIRST_POLL_DELAY, EVENT_POLL);
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut last_format_seq = 0u64;
        let mut last_sources = None;
        let mut new_source_pass_at = None;
        let mut polls = 0u32;
        let mut events = SessionEvents::default();
        loop {
            tokio::select! {
                _ = &mut dropped => break,
                _ = interval.tick() => {}
            }
            let Some(poll) = self.state.lock().audio.as_ref().map(NativePoll::read) else {
                break;
            };
            let (format, sources) = (poll.format, poll.sources);
            for message in events.update(self.handle, poll) {
                send_root_audio_event(server, self.handle, message);
            }
            if format.seq != last_format_seq {
                last_format_seq = format.seq;
                send_root_audio_event(
                    server,
                    self.handle,
                    proto::RootAudioCaptureFormat {
                        sample_rate: format.sample_rate,
                        channels: format.channels,
                    }
                    .into(),
                );
            }
            polls = polls.wrapping_add(1);
            if last_sources.is_some_and(|last| last != sources) {
                new_source_pass_at = Some(polls.wrapping_add(NEW_SOURCE_PASS_DELAY_POLLS));
            }
            last_sources = Some(sources);
            if new_source_pass_at == Some(polls) {
                new_source_pass_at = None;
                self.clone().spawn_pass(server, Pass::NewSource);
            } else if polls % RESOLVE_EVERY_POLLS == 0 {
                self.clone().spawn_pass(server, Pass::Periodic);
            }
        }
    }

    /// One periodic pass at a time; a new stream's pass always runs (handles
    /// already being resolved are run again rather than resolved twice).
    fn spawn_pass(self: Arc<Self>, server: &'static FfiServer, pass: Pass) {
        let periodic = pass == Pass::Periodic;
        if periodic && self.periodic_pass.swap(true, Ordering::AcqRel) {
            return;
        }
        let task = server.async_runtime.spawn(async move {
            self.resolve_pass(server, pass).await;
            if periodic {
                self.periodic_pass.store(false, Ordering::Release);
            }
        });
        server.watch_panic(task);
    }

    /// Forgets the gains of track handles that are gone and resolves SSRCs:
    /// unknown ones every pass, known ones every few passes or after a new
    /// stream appeared.
    async fn resolve_pass(&self, server: &'static FfiServer, pass: Pass) {
        let (handles, refresh_known) = {
            let mut state = self.state.lock();
            let refresh_known = match pass {
                Pass::NewSource => true,
                Pass::Periodic => {
                    state.resolve_passes = state.resolve_passes.wrapping_add(1);
                    state.resolve_passes % KNOWN_RESOLVE_EVERY_PASSES == 0
                }
            };
            (state.gains.handles(), refresh_known)
        };
        let mut to_resolve = Vec::new();
        for (handle, known) in handles {
            if server.retrieve_handle::<FfiTrack>(handle).is_err() {
                self.forget(handle);
            } else if !known || refresh_known {
                to_resolve.push(handle);
            }
        }
        if pass == Pass::Periodic {
            prune_subscribed(server);
        }
        self.resolve(to_resolve, pass == Pass::NewSource).await;
    }

    async fn resolve(&self, handles: Vec<FfiHandleId>, supersede: bool) {
        for handle in handles {
            let mut supersede = supersede;
            loop {
                let Some((track, generation)) =
                    self.state.lock().gains.begin_resolve(handle, supersede)
                else {
                    break;
                };
                supersede = false;
                let ssrc = resolve_ssrc(track).await;
                let mut state = self.state.lock();
                let SessionState { audio, gains, .. } = &mut *state;
                if !gains.finish_resolve(audio.as_ref().map(sink_of), handle, generation, ssrc) {
                    break;
                }
            }
        }
    }

    fn spawn_resolve(self: &Arc<Self>, server: &'static FfiServer, handles: Vec<FfiHandleId>) {
        if handles.is_empty() {
            return;
        }
        let session = self.clone();
        let task = server.async_runtime.spawn(async move { session.resolve(handles, false).await });
        server.watch_panic(task);
    }

    fn forget(&self, handle: FfiHandleId) {
        let mut state = self.state.lock();
        let SessionState { audio, gains, .. } = &mut *state;
        gains.forget(audio.as_ref().map(sink_of), handle);
    }

    /// Stores the gains and applies the ones whose SSRC is known; returns the
    /// handles still to resolve, or Err when the session has ended.
    fn set_gains(&self, requests: Vec<GainRequest>) -> Result<Vec<FfiHandleId>, ()> {
        let subscribed: Vec<Option<u32>> = {
            let sessions = SESSIONS.lock();
            requests.iter().map(|r| sessions.subscribed.get(&r.handle).copied()).collect()
        };
        let mut state = self.state.lock();
        let SessionState { audio, gains, .. } = &mut *state;
        let Some(audio) = audio.as_ref() else {
            return Err(());
        };
        let mut unresolved = Vec::new();
        for (request, subscribed) in requests.into_iter().zip(subscribed) {
            let handle = request.handle;
            if gains.set(audio, handle, request.track, request.tile, request.paused, subscribed) {
                unresolved.push(handle);
            }
        }
        Ok(unresolved)
    }
}

/// A track was subscribed. While a session is live, a remote audio track's
/// SSRC is resolved right away, so the host's first gain for it applies inside
/// its request.
pub fn on_track_subscribed(server: &'static FfiServer, handle: FfiHandleId, track: &Track) {
    let Track::RemoteAudio(track) = track else {
        return;
    };
    if SESSIONS.lock().live.is_empty() {
        return;
    }
    let track = track.clone();
    let task = server.async_runtime.spawn(async move {
        if let Some(ssrc) = resolve_ssrc(track).await {
            let mut sessions = SESSIONS.lock();
            if !sessions.live.is_empty() {
                sessions.subscribed.insert(handle, ssrc);
            }
        }
    });
    server.watch_panic(task);
}

fn prune_subscribed(server: &'static FfiServer) {
    let handles: Vec<FfiHandleId> = SESSIONS.lock().subscribed.keys().copied().collect();
    let gone: Vec<FfiHandleId> = handles
        .into_iter()
        .filter(|handle| server.retrieve_handle::<FfiTrack>(*handle).is_err())
        .collect();
    if !gone.is_empty() {
        let mut sessions = SESSIONS.lock();
        for handle in gone {
            sessions.subscribed.remove(&handle);
        }
    }
}

/// The SSRC of the stream `track` plays: the receiver's signaled SSRC first (a
/// hop to libwebrtc's worker thread, off the async runtime), else the track's
/// inbound-rtp SSRC.
async fn resolve_ssrc(track: RemoteAudioTrack) -> Option<u32> {
    let signaled = {
        let track = track.clone();
        tokio::task::spawn_blocking(move || track.signaled_ssrc()).await.ok().flatten()
    };
    if signaled.is_some() {
        return signaled;
    }
    let stats = track.get_stats().await.ok()?;
    stats.iter().find_map(|stats| match stats {
        RtcStats::InboundRtp(inbound) if inbound.stream.ssrc != 0 => Some(inbound.stream.ssrc),
        _ => None,
    })
}

fn session_from_handle(
    server: &'static FfiServer,
    handle: FfiHandleId,
) -> FfiResult<Arc<RootAudioSession>> {
    Ok(server.retrieve_handle::<FfiPlatformAudio>(handle)?.rootapp.clone())
}

fn remote_audio_track(server: &'static FfiServer, handle: FfiHandleId) -> Option<RemoteAudioTrack> {
    match &server.retrieve_handle::<FfiTrack>(handle).ok()?.track {
        Track::RemoteAudio(track) => Some(track.clone()),
        _ => None,
    }
}

/// The proto's control, with only the fields that are set.
pub fn root_audio_control(control: &proto::RootAudioControl) -> RootAudioControl {
    RootAudioControl {
        mic_muted: control.mic_muted,
        push_to_talk: control.push_to_talk.map(PushToTalk::from_wire),
        input_volume: control.input_volume,
        master_volume: control.master_volume,
        deafened: control.deafened,
        echo_cancellation: control.echo_cancellation,
        webrtc_noise_suppression: control.webrtc_noise_suppression,
        denoise_power: control.denoise_power,
        auto_gain: control.auto_gain,
        gate_threshold: control.gate_threshold,
        auto_sensitivity: control.auto_sensitivity,
        gain_reset_db: control.gain_reset_db,
        denoiser_wanted: control.denoiser_wanted,
    }
    .sanitized()
}

/// An effect clip from the host's memory: `sample_count` int16 samples at
/// `data_ptr`, which must stay valid for the duration of the request.
fn effect_samples(req: &proto::RootAudioRegisterEffectRequest) -> Result<&[i16], String> {
    if req.data_ptr == 0 || req.sample_count == 0 {
        return Err("effect clip is empty".into());
    }
    if req.sample_count > MAX_EFFECT_SAMPLES {
        return Err(format!("effect clip of {} samples is longer than 60 s", req.sample_count));
    }
    if req.data_ptr % std::mem::align_of::<i16>() as u64 != 0 {
        return Err("effect clip pointer is not aligned for int16".into());
    }
    // SAFETY: the host keeps `sample_count` int16 samples at `data_ptr` alive and
    // unchanged until the request returns (checked non-null, aligned, bounded).
    Ok(unsafe {
        std::slice::from_raw_parts(req.data_ptr as usize as *const i16, req.sample_count as usize)
    })
}

fn device_selection(selection: proto::RootAudioDeviceSelection) -> AudioDeviceSelection {
    use proto::root_audio_device_selection::Device;
    match selection.device {
        Some(Device::Guid(guid)) => AudioDeviceSelection::Guid(guid),
        Some(Device::Name(name)) => AudioDeviceSelection::Name(name),
        Some(Device::SystemDefault(_)) | None => AudioDeviceSelection::SystemDefault,
    }
}

fn send_root_audio_event(
    server: &'static FfiServer,
    platform_audio_handle: u64,
    message: proto::root_audio_event::Message,
) {
    let event = proto::RootAudioEvent { platform_audio_handle, message: Some(message) };
    if let Err(e) = server.send_event(event.into()) {
        log::warn!("failed to send RootAudioEvent: {}", e);
    }
}

fn spawn_device_result(
    server: &'static FfiServer,
    platform_audio_handle: u64,
    async_id: u64,
    results: impl std::future::Future<Output = AudioDeviceResults> + Send + 'static,
) {
    let handle = server.async_runtime.spawn(async move {
        let results = results.await;
        let error = |result: Option<livekit::AudioResult<()>>| {
            result.and_then(|r| r.err()).map(|e| e.to_string())
        };
        send_root_audio_event(
            server,
            platform_audio_handle,
            proto::RootAudioDeviceResult {
                async_id,
                recording_error: error(results.recording),
                playout_error: error(results.playout),
            }
            .into(),
        );
    });
    server.watch_panic(handle);
}

pub fn on_select_devices(
    server: &'static FfiServer,
    req: proto::RootAudioSelectDevicesRequest,
) -> FfiResult<proto::RootAudioSelectDevicesResponse> {
    let audio = platform_audio_from_handle(server, req.platform_audio_handle)?;
    let async_id = server.next_id();
    let results = audio
        .select_devices(req.recording.map(device_selection), req.playout.map(device_selection));
    spawn_device_result(server, req.platform_audio_handle, async_id, results);
    Ok(proto::RootAudioSelectDevicesResponse { async_id })
}

pub fn on_restart_device(
    server: &'static FfiServer,
    req: proto::RootAudioRestartDeviceRequest,
) -> FfiResult<proto::RootAudioRestartDeviceResponse> {
    let audio = platform_audio_from_handle(server, req.platform_audio_handle)?;
    let async_id = server.next_id();
    let results = audio.restart_devices(req.recording, req.playout);
    spawn_device_result(server, req.platform_audio_handle, async_id, results);
    Ok(proto::RootAudioRestartDeviceResponse { async_id })
}

pub fn on_set_playout_hold(
    server: &'static FfiServer,
    req: proto::RootAudioSetPlayoutHoldRequest,
) -> FfiResult<proto::RootAudioSetPlayoutHoldResponse> {
    platform_audio_from_handle(server, req.platform_audio_handle)?.set_playout_hold(req.hold);
    Ok(proto::RootAudioSetPlayoutHoldResponse { error: None })
}

pub fn on_stats(
    server: &'static FfiServer,
    req: proto::RootAudioStatsRequest,
) -> FfiResult<proto::RootAudioStatsResponse> {
    let stats =
        platform_audio_from_handle(server, req.platform_audio_handle)?.rootapp_audio_stats();
    Ok(proto::RootAudioStatsResponse { stats: Some(stats.into()), error: None })
}

/// Every field is sanitised (finite, clamped; an unknown push-to-talk value
/// closes the gate). mic_muted, push_to_talk, input_volume, master_volume and
/// deafened take effect at once; echo_cancellation and
/// webrtc_noise_suppression within a second (rate-limited APM policy, and with
/// the DSP chain at a pause in speech). denoise_power, auto_gain,
/// gate_threshold, auto_sensitivity, gain_reset_db and denoiser_wanted are
/// read by the DSP chain on its next frame. A field this build cannot act on
/// (no capture processor or APM decorator) is reported as not supported; the
/// others are applied.
pub fn on_set_control(
    server: &'static FfiServer,
    req: proto::RootAudioSetControlRequest,
) -> FfiResult<proto::RootAudioSetControlResponse> {
    let audio = platform_audio_from_handle(server, req.platform_audio_handle)?;
    let error = audio
        .set_rootapp_control(&root_audio_control(&req.control))
        .err()
        .map(|_| format!("a control field is {NOT_SUPPORTED}; the other fields are applied"));
    Ok(proto::RootAudioSetControlResponse { error })
}

/// Tile volumes (slider / 100, 0..2, NaN is 0) per remote track handle; an
/// absent `paused` keeps the track's pause. Handles that are not remote audio
/// tracks (a track disposed meanwhile) are listed in `unknown_track_handles`
/// and are no error; the other gains are applied.
pub fn on_set_track_gains(
    server: &'static FfiServer,
    req: proto::RootAudioSetTrackGainsRequest,
) -> FfiResult<proto::RootAudioSetTrackGainsResponse> {
    let session = session_from_handle(server, req.platform_audio_handle)?;
    let mut unknown_track_handles = Vec::new();
    let mut requests = Vec::with_capacity(req.gains.len());
    for gain in &req.gains {
        match remote_audio_track(server, gain.track_handle) {
            Some(track) => requests.push(GainRequest {
                handle: gain.track_handle,
                track,
                tile: sanitize_volume(gain.gain),
                paused: gain.paused,
            }),
            None => unknown_track_handles.push(gain.track_handle),
        }
    }
    if !unknown_track_handles.is_empty() {
        log::debug!("RootAudio: gains for track handles that are gone: {unknown_track_handles:?}");
    }
    let error = match session.set_gains(requests) {
        Ok(unresolved) => {
            session.spawn_resolve(server, unresolved);
            None
        }
        Err(()) => Some("platform audio handle was dropped".into()),
    };
    Ok(proto::RootAudioSetTrackGainsResponse { error, unknown_track_handles })
}

/// Copies the clip before returning.
pub fn on_register_effect(
    server: &'static FfiServer,
    req: proto::RootAudioRegisterEffectRequest,
) -> FfiResult<proto::RootAudioRegisterEffectResponse> {
    let audio = platform_audio_from_handle(server, req.platform_audio_handle)?;
    let error = match effect_samples(&req) {
        Ok(samples) => audio.register_effect(req.effect_id, samples).err().map(|e| e.to_string()),
        Err(e) => Some(e),
    };
    Ok(proto::RootAudioRegisterEffectResponse { error })
}

/// A full queue is not an error (the effect is counted in effects_dropped); an
/// unregistered id is.
pub fn on_play_effect(
    server: &'static FfiServer,
    req: proto::RootAudioPlayEffectRequest,
) -> FfiResult<proto::RootAudioPlayEffectResponse> {
    let audio = platform_audio_from_handle(server, req.platform_audio_handle)?;
    let error = match audio.play_effect(req.effect_id, req.gain) {
        EffectPlay::UnknownClip => Some(format!("effect {} is not registered", req.effect_id)),
        EffectPlay::Queued | EffectPlay::Dropped | EffectPlay::Idle => None,
    };
    Ok(proto::RootAudioPlayEffectResponse { error })
}

pub fn on_stop_effects(
    server: &'static FfiServer,
    req: proto::RootAudioStopEffectsRequest,
) -> FfiResult<proto::RootAudioStopEffectsResponse> {
    platform_audio_from_handle(server, req.platform_audio_handle)?.stop_effects();
    Ok(proto::RootAudioStopEffectsResponse { error: None })
}

/// Why a SetDenoiser request fails before it is queued: an install needs the
/// Effects SDK slot, an aligned pointer and a rate the chain runs (8-48 kHz).
/// A removal without a slot has nothing to remove and succeeds.
fn denoiser_request_error(available: bool, pipeline: u64, sample_rate: u32) -> Option<String> {
    if pipeline == 0 {
        return None;
    }
    if !available {
        return Some(format!("the denoiser is {NOT_SUPPORTED}"));
    }
    if pipeline % std::mem::align_of::<usize>() as u64 != 0 {
        return Some("pipeline pointer is not aligned".into());
    }
    if !(MIN_DENOISER_RATE..=MAX_DENOISER_RATE).contains(&sample_rate) {
        return Some(format!(
            "pipeline rate {sample_rate} is outside {MIN_DENOISER_RATE}-{MAX_DENOISER_RATE} Hz"
        ));
    }
    None
}

/// Queues the pipeline for the capture processor (installed at a pause in
/// speech, at most 3 s later; 0 removes at once). The result arrives as
/// RootAudioEvent.denoiser_result; an error before queueing is sent at once.
pub fn on_set_denoiser(
    server: &'static FfiServer,
    req: proto::RootAudioSetDenoiserRequest,
) -> FfiResult<proto::RootAudioSetDenoiserResponse> {
    let audio = platform_audio_from_handle(server, req.platform_audio_handle)?;
    let async_id = server.next_id();
    let handle = req.platform_audio_handle;
    let available = audio.denoiser_available();
    let mut error = denoiser_request_error(available, req.pipeline_ptr, req.sample_rate);
    let queued = error.is_none() && available;
    if queued {
        if let Err(e) = audio.set_denoiser(handle, async_id, req.pipeline_ptr, req.sample_rate) {
            error = Some(e.to_string());
        }
    }
    if !queued || error.is_some() {
        let task = server.async_runtime.spawn(async move {
            send_root_audio_event(
                server,
                handle,
                proto::RootAudioDenoiserResult { async_id, error }.into(),
            );
        });
        server.watch_panic(task);
    }
    Ok(proto::RootAudioSetDenoiserResponse { async_id })
}

/// Synchronous: every pipeline native holds (installed, waiting to be, or
/// returned and not sent yet). Waiting requests complete with an error
/// event; the installed pipeline is removed unless the audio thread holds on
/// for 200 ms (then it stays and `error` says so).
pub fn on_return_pipelines(
    server: &'static FfiServer,
    req: proto::RootAudioReturnPipelinesRequest,
) -> FfiResult<proto::RootAudioReturnPipelinesResponse> {
    let audio = platform_audio_from_handle(server, req.platform_audio_handle)?;
    if !audio.denoiser_available() {
        return Ok(proto::RootAudioReturnPipelinesResponse { pipelines: Vec::new(), error: None });
    }
    let held = audio.return_denoiser_pipelines();
    let completions = audio.take_denoiser_completions();
    let returned = audio.take_returned_pipelines();
    let undelivered = {
        let mut sessions = SESSIONS.lock();
        sessions.deliver(completions, returned);
        sessions.mail.take_undelivered()
    };
    let mut pipelines = held.pipelines;
    for pipeline in undelivered {
        if !pipelines.contains(&pipeline) {
            pipelines.push(pipeline);
        }
    }
    Ok(proto::RootAudioReturnPipelinesResponse { pipelines, error: held.error })
}

pub fn on_set_monitor(
    _server: &'static FfiServer,
    _req: proto::RootAudioSetMonitorRequest,
) -> FfiResult<proto::RootAudioSetMonitorResponse> {
    Ok(proto::RootAudioSetMonitorResponse { error: Some(NOT_SUPPORTED.into()) })
}

impl From<RootAudioStats> for proto::RootAudioStats {
    fn from(stats: RootAudioStats) -> Self {
        Self {
            capture_callbacks: stats.capture_callbacks,
            render_callbacks: stats.render_callbacks,
            capture_max_gap_us: stats.capture_max_gap_us,
            render_max_gap_us: stats.render_max_gap_us,
            last_capture_age_us: stats.last_capture_age_us,
            last_render_age_us: stats.last_render_age_us,
            recording: stats.recording,
            playing: stats.playing,
            recording_init_result: stats.recording_init_result,
            recording_start_result: stats.recording_start_result,
            playout_init_result: stats.playout_init_result,
            playout_start_result: stats.playout_start_result,
            capture_rate: stats.capture_rate,
            capture_channels: stats.capture_channels,
            capture_raw_sumsq: stats.capture_raw_sumsq,
            capture_raw_samples: stats.capture_raw_samples,
            playout_delay_ms: stats.playout_delay_ms,
            recording_delay_ms: stats.recording_delay_ms,
            mix_calls: stats.mix_calls,
            mix_max_gap_us: stats.mix_max_gap_us,
            mix_sources: stats.mix_sources,
            mix_ssrcs: stats.mix_ssrcs,
            mix_out_sumsq: stats.mix_out_sumsq,
            mix_out_samples: stats.mix_out_samples,
            effects_played: stats.effects_played,
            effects_dropped: stats.effects_dropped,
            processed_frames: stats.processed_frames,
            process_rate: stats.process_rate,
            process_channels: stats.process_channels,
            apm_out_sumsq: stats.apm_out_sumsq,
            denoised_sumsq: stats.denoised_sumsq,
            gate_in_sumsq: stats.gate_in_sumsq,
            sent_sumsq: stats.sent_sumsq,
            stage_samples: stats.stage_samples,
            gate_in_block_peak_db: stats.gate_in_block_peak_db,
            threshold: stats.threshold,
            envelope: stats.envelope,
            gate_speaking: stats.gate_speaking,
            speaking_seq: stats.speaking_seq,
            voice_gain_db: stats.voice_gain_db,
            voice_gain_settled: stats.voice_gain_settled,
            gate_open_frames: stats.gate_open_frames,
            denoiser_installed: stats.denoiser_installed,
            denoiser_frames: stats.denoiser_frames,
            denoiser_misses: stats.denoiser_misses,
            denoiser_empty_outputs: stats.denoiser_empty_outputs,
            denoiser_process_max_us: stats.denoiser_process_max_us,
            denoiser_slow_frames: stats.denoiser_slow_frames,
            denoiser_bypass_reason: stats.denoiser_bypass_reason,
            denoiser_notifications: stats.denoiser_notifications,
            erl_db: stats.erl_db,
            erle_db: stats.erle_db,
            aec_delay_ms: stats.aec_delay_ms,
            divergent_filter_fraction: stats.divergent_filter_fraction,
            echo_cancellation_active: stats.echo_cancellation_active,
            webrtc_ns_active: stats.webrtc_ns_active,
            audio_thread_lock_violations: Some(stats.audio_thread_lock_violations),
            audio_thread_logs_dropped: Some(stats.audio_thread_logs_dropped),
            overlapping_callbacks: Some(stats.overlapping_callbacks),
            device_stop_failures: Some(stats.device_stop_failures),
            speaking: stats.speaking,
            gate_input_above_frames: stats.gate_input_above_frames,
            startup_hold_frames: stats.startup_hold_frames,
            transmit_frames: stats.transmit_frames,
            unsupported_frames: stats.unsupported_frames,
            non_finite_samples: stats.non_finite_samples,
            dsp_resets: stats.dsp_resets,
            dsp_flushes: stats.dsp_flushes,
            gate_open_delay_us: stats.gate_open_delay_us,
            ns_gap_frames: stats.ns_gap_frames,
            denoiser_rate: stats.denoiser_rate,
            denoiser_installs: stats.denoiser_installs,
            denoiser_bypassed_frames: stats.denoiser_bypassed_frames,
            denoiser_process_us_buckets: stats.denoiser_process_us_buckets,
            denoiser_other_notifications: stats.denoiser_other_notifications,
            denoiser_latency_ms: stats.denoiser_latency_ms,
            denoiser_buffered_samples: stats.denoiser_buffered_samples,
            denoiser_underruns: stats.denoiser_underruns,
            denoiser_padded_samples: stats.denoiser_padded_samples,
            denoiser_trimmed_samples: stats.denoiser_trimmed_samples,
            denoiser_drained_samples: stats.denoiser_drained_samples,
            denoiser_frames_until_first_output: stats.denoiser_frames_until_first_output,
            webrtc_ns_fallback: stats.webrtc_ns_fallback,
            apm_policy_applies: stats.apm_policy_applies,
            apm_policy_deferred: stats.apm_policy_deferred,
            apm_policy_forced: stats.apm_policy_forced,
            apm_policy_pending: stats.apm_policy_pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rootapp_audio_caps_cover_phase_1() {
        #[cfg(not(any(target_os = "ios", target_os = "android")))]
        {
            assert_eq!(ROOTAPP_AUDIO_CAPS, 0b1000_1111_1111);
            assert_eq!(
                ROOTAPP_AUDIO_CAPS & (CAP_TRACK_GAINS | CAP_EFFECTS | CAP_CONTROL),
                CAP_TRACK_GAINS | CAP_EFFECTS | CAP_CONTROL
            );
            assert_eq!(ROOTAPP_AUDIO_CAPS & CAP_APM_DECORATOR, CAP_APM_DECORATOR);
        }
        assert_eq!(crate::cabi::livekit_ffi_rootapp_audio_caps(), ROOTAPP_AUDIO_CAPS);
        assert_eq!(ROOTAPP_AUDIO_CAPS & (CAP_DSP | CAP_DENOISER | CAP_MONITOR), 0);
    }

    #[test]
    fn rootapp_audio_denoiser_notification_handler_is_exported_on_64_bit_desktop() {
        let handler = crate::cabi::livekit_ffi_rootapp_denoiser_notification_handler();
        let expected = cfg!(target_pointer_width = "64")
            && !cfg!(any(target_os = "ios", target_os = "android"));
        assert_eq!(!handler.is_null(), expected);
        assert_eq!(handler, crate::cabi::livekit_ffi_rootapp_denoiser_notification_handler());
    }

    #[test]
    fn rootapp_audio_control_keeps_unset_fields_unset() {
        let control = root_audio_control(&proto::RootAudioControl {
            mic_muted: Some(true),
            ..Default::default()
        });
        assert_eq!(control, RootAudioControl { mic_muted: Some(true), ..Default::default() });
    }

    #[test]
    fn rootapp_audio_control_sanitises_every_field() {
        let control = root_audio_control(&proto::RootAudioControl {
            mic_muted: Some(false),
            push_to_talk: Some(9),
            input_volume: Some(f32::NAN),
            master_volume: Some(f32::INFINITY),
            deafened: Some(true),
            echo_cancellation: Some(false),
            webrtc_noise_suppression: Some(true),
            denoise_power: Some(f32::NEG_INFINITY),
            auto_gain: Some(false),
            gate_threshold: Some(3.0),
            auto_sensitivity: Some(true),
            gain_reset_db: Some(f32::NAN),
            denoiser_wanted: Some(true),
        });
        assert_eq!(control.denoiser_wanted, Some(true));
        assert_eq!(control.push_to_talk, Some(PushToTalk::Released));
        assert_eq!(control.input_volume, Some(0.0));
        assert_eq!(control.master_volume, Some(2.0));
        assert_eq!(control.denoise_power, Some(0.0));
        assert_eq!(control.gate_threshold, Some(1.0));
        assert_eq!(control.gain_reset_db, None);
        assert_eq!(
            (control.mic_muted, control.deafened, control.echo_cancellation),
            (Some(false), Some(true), Some(false))
        );
        assert_eq!(
            (control.webrtc_noise_suppression, control.auto_gain, control.auto_sensitivity),
            (Some(true), Some(false), Some(true))
        );

        let push_to_talk = |value| {
            root_audio_control(&proto::RootAudioControl {
                push_to_talk: Some(value),
                ..Default::default()
            })
            .push_to_talk
        };
        assert_eq!(push_to_talk(0), Some(PushToTalk::VoiceActivity));
        assert_eq!(push_to_talk(1), Some(PushToTalk::Held));
        assert_eq!(push_to_talk(2), Some(PushToTalk::Released));
    }

    fn effect_request(data_ptr: u64, sample_count: u32) -> proto::RootAudioRegisterEffectRequest {
        proto::RootAudioRegisterEffectRequest {
            platform_audio_handle: 1,
            effect_id: 7,
            data_ptr,
            sample_count,
        }
    }

    #[test]
    fn rootapp_audio_effect_clips_are_checked_before_they_are_read() {
        let clip: Vec<i16> = (0..480).map(|i| i as i16).collect();
        let ptr = clip.as_ptr() as usize as u64;
        let request = effect_request(ptr, 480);
        assert_eq!(effect_samples(&request).unwrap(), clip.as_slice());

        assert!(effect_samples(&effect_request(0, 480)).is_err());
        assert!(effect_samples(&effect_request(ptr, 0)).is_err());
        assert!(effect_samples(&effect_request(ptr, MAX_EFFECT_SAMPLES + 1)).is_err());
        assert!(effect_samples(&effect_request(ptr + 1, 4)).is_err());
    }

    #[test]
    fn rootapp_audio_device_selection_maps_every_case() {
        use proto::root_audio_device_selection::Device;
        let selection = |device| proto::RootAudioDeviceSelection { device };
        assert_eq!(
            device_selection(selection(Some(Device::SystemDefault(true)))),
            AudioDeviceSelection::SystemDefault
        );
        assert_eq!(device_selection(selection(None)), AudioDeviceSelection::SystemDefault);
        assert_eq!(
            device_selection(selection(Some(Device::Guid("{0.0.1}".into())))),
            AudioDeviceSelection::Guid("{0.0.1}".into())
        );
        assert_eq!(
            device_selection(selection(Some(Device::Name("Headset".into())))),
            AudioDeviceSelection::Name("Headset".into())
        );
    }

    #[test]
    fn rootapp_audio_stats_convert_field_for_field() {
        let stats = RootAudioStats {
            capture_callbacks: 3,
            render_callbacks: 4,
            mix_ssrcs: vec![9],
            playout_start_result: -1,
            audio_thread_lock_violations: 1,
            overlapping_callbacks: 2,
            ..Default::default()
        };
        let proto: proto::RootAudioStats = stats.into();
        assert_eq!(proto.capture_callbacks, 3);
        assert_eq!(proto.render_callbacks, 4);
        assert_eq!(proto.mix_ssrcs, vec![9]);
        assert_eq!(proto.playout_start_result, -1);
        assert!(proto.erle_db.is_nan());
        assert_eq!(proto.audio_thread_lock_violations, Some(1));
        assert_eq!(proto.audio_thread_logs_dropped, Some(0));
        assert_eq!(proto.overlapping_callbacks, Some(2));
        assert_eq!(proto.device_stop_failures, Some(0));
        assert_eq!(proto::RootAudioStats::from(RootAudioStats::default()).aec_delay_ms, -1);
    }

    /// Records what a session asks of the native engine.
    #[derive(Default)]
    struct Recorder {
        calls: std::cell::RefCell<Vec<String>>,
    }

    impl Recorder {
        fn take(&self) -> Vec<String> {
            std::mem::take(&mut *self.calls.borrow_mut())
        }
    }

    impl RootAudioSink for Recorder {
        fn set_source_gain(&self, ssrc: u32, tile: f32, paused: bool) -> bool {
            self.calls.borrow_mut().push(format!("set {ssrc} {tile} {paused}"));
            true
        }

        fn clear_source_gain(&self, ssrc: u32) {
            self.calls.borrow_mut().push(format!("clear {ssrc}"));
        }

        fn reset_controls(&self, close_gate: bool) {
            self.calls.borrow_mut().push(format!("reset closed={close_gate}"));
        }
    }

    /// `handle` resolved to `ssrc` (track `handle`).
    fn resolved(gains: &mut TrackGains<u64>, sink: &Recorder, handle: u64, ssrc: u32) {
        let (_, generation) = gains.begin_resolve(handle, false).unwrap();
        assert!(!gains.finish_resolve(Some(sink), handle, generation, Some(ssrc)));
    }

    #[test]
    fn rootapp_audio_sessions_reset_on_the_first_start_and_close_on_the_last_end() {
        let sink = Recorder::default();
        let mut sessions = Sessions::default();
        sessions.open(1, &sink);
        assert_eq!(sink.take(), ["reset closed=false"]);
        sessions.open(2, &sink);
        sessions.subscribed.insert(9, 99);
        sessions.close(1, &sink);
        assert_eq!(sink.take(), Vec::<String>::new(), "another handle is still live");
        assert_eq!(sessions.subscribed.len(), 1);
        sessions.close(1, &sink);
        assert_eq!(sink.take(), Vec::<String>::new(), "a handle closes once");
        sessions.close(2, &sink);
        assert_eq!(sink.take(), ["reset closed=true"], "the last end closes the gate");
        assert!(sessions.subscribed.is_empty());
        sessions.open(3, &sink);
        assert_eq!(sink.take(), ["reset closed=false"]);
    }

    fn completion(owner: u64, async_id: u64, error: Option<&str>) -> DenoiserCompletion {
        DenoiserCompletion { owner, async_id, error: error.map(str::to_string) }
    }

    fn returned(owner: u64, pipeline: u64) -> ReturnedPipeline {
        ReturnedPipeline { owner, pipeline }
    }

    #[test]
    fn rootapp_audio_denoiser_results_go_to_the_handle_that_asked() {
        let sink = Recorder::default();
        let mut sessions = Sessions::default();
        sessions.open(1, &sink);
        sessions.open(2, &sink);
        sessions.deliver(
            vec![completion(1, 10, None), completion(2, 11, Some("busy")), completion(9, 12, None)],
            Vec::new(),
        );
        assert_eq!(sessions.mail.take(1), (vec![(10, None)], Vec::new()));
        assert_eq!(sessions.mail.take(2), (vec![(11, Some("busy".to_string()))], Vec::new()));
        assert_eq!(sessions.mail.take(1), (Vec::new(), Vec::new()), "each once");

        sessions.deliver(vec![completion(2, 13, None)], Vec::new());
        sessions.close(2, &sink);
        assert_eq!(
            sessions.mail.take(2),
            (Vec::new(), Vec::new()),
            "a dropped handle's results go"
        );
        assert_eq!(sessions.mail.take(1), (Vec::new(), Vec::new()));
    }

    #[test]
    fn rootapp_audio_returned_pipelines_reach_exactly_one_handle() {
        let sink = Recorder::default();
        let mut sessions = Sessions::default();
        sessions.open(1, &sink);
        sessions.open(5, &sink);
        sessions.deliver(Vec::new(), vec![returned(1, 100), returned(7, 101), returned(5, 102)]);
        assert_eq!(sessions.mail.take(1).1, [100], "to the owner while it lives");
        assert_eq!(sessions.mail.take(5).1, [101, 102], "else to the newest live handle");

        sessions.deliver(Vec::new(), vec![returned(1, 103)]);
        sessions.close(1, &sink);
        assert_eq!(sessions.mail.take(5).1, [103], "a closed handle's pipelines move on");

        sessions.deliver(Vec::new(), vec![returned(5, 104)]);
        sessions.close(5, &sink);
        sessions.deliver(Vec::new(), vec![returned(5, 105)]);
        assert_eq!(sessions.mail.take(5).1, Vec::<u64>::new());
        sessions.open(6, &sink);
        assert_eq!(sessions.mail.take(6).1, [104, 105], "kept for the next handle");
    }

    #[test]
    fn rootapp_audio_return_pipelines_takes_every_undelivered_pipeline() {
        let sink = Recorder::default();
        let mut sessions = Sessions::default();
        sessions.deliver(Vec::new(), vec![returned(1, 100)]);
        sessions.open(2, &sink);
        sessions.open(3, &sink);
        sessions.deliver(Vec::new(), vec![returned(3, 101), returned(2, 102)]);
        let mut undelivered = sessions.mail.take_undelivered();
        undelivered.sort_unstable();
        assert_eq!(undelivered, [100, 101, 102]);
        assert_eq!(sessions.mail.take(2).1, Vec::<u64>::new());
        assert_eq!(sessions.mail.take(3).1, Vec::<u64>::new());
        assert!(sessions.mail.take_undelivered().is_empty());
    }

    fn speaking(speaking: bool, seq: u64) -> LocalSpeaking {
        LocalSpeaking { speaking, seq }
    }

    fn state(installed: bool, rate: u32, bypass_reason: u32, fallback: bool) -> DenoiserState {
        DenoiserState { installed, rate, bypass_reason, webrtc_ns_fallback: fallback }
    }

    #[test]
    fn rootapp_audio_speaking_is_sent_on_every_change_per_handle() {
        use proto::root_audio_event::Message;
        let mut events = SessionEvents::default();
        assert!(events.messages(speaking(false, 0), None, Vec::new(), Vec::new()).is_empty());
        assert_eq!(
            events.messages(speaking(true, 3), None, Vec::new(), Vec::new()),
            [Message::Speaking(proto::RootAudioSpeaking { speaking: true, seq: 3 })]
        );
        assert!(events.messages(speaking(true, 5), None, Vec::new(), Vec::new()).is_empty());
        assert_eq!(
            events.messages(speaking(false, 6), None, Vec::new(), Vec::new()),
            [Message::Speaking(proto::RootAudioSpeaking { speaking: false, seq: 6 })]
        );

        let mut late = SessionEvents::default();
        assert_eq!(
            late.messages(speaking(true, 9), None, Vec::new(), Vec::new()),
            [Message::Speaking(proto::RootAudioSpeaking { speaking: true, seq: 9 })],
            "a new handle hears the current value when it is true"
        );
    }

    #[test]
    fn rootapp_audio_denoiser_state_is_sent_first_and_on_change() {
        use proto::root_audio_event::Message;
        let mut events = SessionEvents::default();
        let quiet = speaking(false, 0);
        let empty = state(false, 0, 0, false);
        assert_eq!(
            events.messages(quiet, Some(empty), Vec::new(), Vec::new()),
            [Message::DenoiserState(proto::RootAudioDenoiserState {
                installed: false,
                rate: 0,
                bypass_reason: 0,
                webrtc_ns_fallback: false
            })]
        );
        assert!(events.messages(quiet, Some(empty), Vec::new(), Vec::new()).is_empty());
        let stalled = state(true, 48_000, 2, true);
        assert_eq!(events.messages(quiet, Some(stalled), Vec::new(), Vec::new()).len(), 1);
        assert!(events.messages(quiet, None, Vec::new(), Vec::new()).is_empty());

        let messages = events.messages(
            quiet,
            Some(stalled),
            vec![(4, None), (5, Some("x".into()))],
            vec![7, 8],
        );
        assert_eq!(
            messages,
            [
                Message::DenoiserResult(proto::RootAudioDenoiserResult {
                    async_id: 4,
                    error: None
                }),
                Message::DenoiserResult(proto::RootAudioDenoiserResult {
                    async_id: 5,
                    error: Some("x".into())
                }),
                Message::PipelinesReturned(proto::RootAudioPipelinesReturned {
                    pipelines: vec![7, 8]
                }),
            ]
        );
    }

    #[test]
    fn rootapp_audio_set_denoiser_validates_before_queueing() {
        let aligned = 0x1000u64;
        assert_eq!(denoiser_request_error(true, aligned, 48_000), None);
        assert_eq!(denoiser_request_error(true, aligned, 16_000), None);
        assert_eq!(denoiser_request_error(true, aligned, 8_000), None);
        assert!(denoiser_request_error(true, aligned, 7_999).is_some());
        assert!(denoiser_request_error(true, aligned, 96_000).is_some());
        assert!(denoiser_request_error(true, aligned + 1, 48_000).is_some());
        assert!(denoiser_request_error(false, aligned, 48_000).unwrap().contains(NOT_SUPPORTED));
        assert_eq!(denoiser_request_error(false, 0, 0), None, "nothing to remove is no error");
        assert_eq!(denoiser_request_error(true, 0, 0), None, "a removal needs no rate");
    }

    #[test]
    fn rootapp_audio_phase_2_stats_convert_field_for_field() {
        let stats = RootAudioStats {
            denoised_sumsq: 1.5,
            gate_in_sumsq: 2.5,
            gate_in_block_peak_db: -12.0,
            threshold: 0.01,
            envelope: 0.25,
            gate_speaking: true,
            speaking_seq: 4,
            voice_gain_db: 3.5,
            voice_gain_settled: true,
            gate_open_frames: 5,
            denoiser_installed: true,
            denoiser_frames: 6,
            denoiser_misses: 7,
            denoiser_empty_outputs: 8,
            denoiser_process_max_us: 9,
            denoiser_slow_frames: 10,
            denoiser_bypass_reason: 2,
            denoiser_notifications: 11,
            speaking: Some(true),
            gate_input_above_frames: Some(12),
            startup_hold_frames: Some(13),
            transmit_frames: Some(14),
            unsupported_frames: Some(15),
            non_finite_samples: Some(16),
            dsp_resets: Some(17),
            dsp_flushes: Some(18),
            gate_open_delay_us: Some(19),
            ns_gap_frames: Some(20),
            denoiser_rate: Some(16_000),
            denoiser_installs: Some(21),
            denoiser_bypassed_frames: Some(22),
            denoiser_process_us_buckets: (0..16).collect(),
            denoiser_other_notifications: Some(23),
            denoiser_latency_ms: Some(24.5),
            denoiser_buffered_samples: Some(25),
            denoiser_underruns: Some(26),
            denoiser_padded_samples: Some(27),
            denoiser_trimmed_samples: Some(28),
            denoiser_drained_samples: Some(29),
            denoiser_frames_until_first_output: Some(30),
            webrtc_ns_fallback: Some(true),
            apm_policy_applies: Some(31),
            apm_policy_deferred: Some(32),
            apm_policy_forced: Some(33),
            apm_policy_pending: Some(false),
            ..Default::default()
        };
        let p: proto::RootAudioStats = stats.into();
        assert_eq!((p.denoised_sumsq, p.gate_in_sumsq), (1.5, 2.5));
        assert_eq!((p.gate_in_block_peak_db, p.threshold, p.envelope), (-12.0, 0.01, 0.25));
        assert!(p.gate_speaking && p.voice_gain_settled && p.denoiser_installed);
        assert_eq!((p.speaking_seq, p.voice_gain_db, p.gate_open_frames), (4, 3.5, 5));
        assert_eq!((p.denoiser_frames, p.denoiser_misses, p.denoiser_empty_outputs), (6, 7, 8));
        assert_eq!((p.denoiser_process_max_us, p.denoiser_slow_frames), (9, 10));
        assert_eq!((p.denoiser_bypass_reason, p.denoiser_notifications), (2, 11));
        assert_eq!(p.speaking, Some(true));
        assert_eq!(
            (p.gate_input_above_frames, p.startup_hold_frames, p.transmit_frames),
            (Some(12), Some(13), Some(14))
        );
        assert_eq!((p.unsupported_frames, p.non_finite_samples), (Some(15), Some(16)));
        assert_eq!((p.dsp_resets, p.dsp_flushes), (Some(17), Some(18)));
        assert_eq!((p.gate_open_delay_us, p.ns_gap_frames), (Some(19), Some(20)));
        assert_eq!(
            (p.denoiser_rate, p.denoiser_installs, p.denoiser_bypassed_frames),
            (Some(16_000), Some(21), Some(22))
        );
        assert_eq!(p.denoiser_process_us_buckets, (0..16).collect::<Vec<u64>>());
        assert_eq!((p.denoiser_other_notifications, p.denoiser_latency_ms), (Some(23), Some(24.5)));
        assert_eq!(
            (p.denoiser_buffered_samples, p.denoiser_underruns, p.denoiser_padded_samples),
            (Some(25), Some(26), Some(27))
        );
        assert_eq!((p.denoiser_trimmed_samples, p.denoiser_drained_samples), (Some(28), Some(29)));
        assert_eq!(
            (p.denoiser_frames_until_first_output, p.webrtc_ns_fallback),
            (Some(30), Some(true))
        );
        assert_eq!(
            (
                p.apm_policy_applies,
                p.apm_policy_deferred,
                p.apm_policy_forced,
                p.apm_policy_pending
            ),
            (Some(31), Some(32), Some(33), Some(false))
        );

        let absent: proto::RootAudioStats = RootAudioStats::default().into();
        assert_eq!(
            (absent.speaking, absent.denoiser_rate, absent.apm_policy_applies),
            (None, None, None)
        );
        assert!(absent.denoiser_process_us_buckets.is_empty());
        assert!(absent.voice_gain_db.is_nan() && absent.gate_in_block_peak_db.is_nan());
    }

    #[test]
    fn rootapp_audio_track_gain_applies_once_its_ssrc_is_resolved() {
        let sink = Recorder::default();
        let mut gains = TrackGains::default();
        assert!(gains.set(&sink, 1, 1, 0.5, Some(true), None));
        assert_eq!(sink.take(), Vec::<String>::new());
        resolved(&mut gains, &sink, 1, 100);
        assert_eq!(sink.take(), ["set 100 0.5 true"]);

        assert!(!gains.set(&sink, 1, 1, 0.75, None, None));
        assert_eq!(sink.take(), ["set 100 0.75 true"], "an absent paused keeps the pause");
        assert!(!gains.set(&sink, 1, 1, 0.75, Some(false), None));
        assert_eq!(sink.take(), ["set 100 0.75 false"]);
        assert_eq!(gains.handles(), [(1, true)]);
    }

    #[test]
    fn rootapp_audio_track_gain_uses_the_ssrc_resolved_at_subscribe() {
        let sink = Recorder::default();
        let mut gains = TrackGains::default();
        assert!(!gains.set(&sink, 1, 1, 1.0, None, Some(55)));
        assert_eq!(sink.take(), ["set 55 1 false"]);
        resolved(&mut gains, &sink, 1, 55);
        assert_eq!(sink.take(), Vec::<String>::new(), "an unchanged SSRC is not set again");
    }

    #[test]
    fn rootapp_audio_handles_sharing_an_ssrc_keep_it_until_the_last_is_forgotten() {
        let sink = Recorder::default();
        let mut gains = TrackGains::default();
        gains.set(&sink, 1, 1, 0.0, None, None);
        gains.set(&sink, 2, 2, 1.0, None, None);
        resolved(&mut gains, &sink, 1, 100);
        resolved(&mut gains, &sink, 2, 100);
        sink.take();
        gains.forget(Some(&sink), 1);
        assert_eq!(sink.take(), Vec::<String>::new(), "a reused transceiver keeps its gain");
        gains.forget(Some(&sink), 2);
        assert_eq!(sink.take(), ["clear 100"]);
    }

    #[test]
    fn rootapp_audio_moved_ssrc_clears_the_old_one_only_when_unused() {
        let sink = Recorder::default();
        let mut gains = TrackGains::default();
        gains.set(&sink, 1, 1, 0.5, None, None);
        gains.set(&sink, 2, 2, 0.25, None, None);
        resolved(&mut gains, &sink, 1, 100);
        resolved(&mut gains, &sink, 2, 100);
        sink.take();

        resolved(&mut gains, &sink, 1, 200);
        assert_eq!(sink.take(), ["set 200 0.5 false"], "handle 2 still plays under 100");
        resolved(&mut gains, &sink, 2, 200);
        assert_eq!(sink.take(), ["clear 100", "set 200 0.25 false"]);
    }

    #[test]
    fn rootapp_audio_resolutions_never_overlap_and_a_superseded_one_is_not_applied() {
        let sink = Recorder::default();
        let mut gains = TrackGains::default();
        gains.set(&sink, 1, 1, 1.0, None, None);

        let (_, first) = gains.begin_resolve(1, false).unwrap();
        assert!(gains.begin_resolve(1, false).is_none(), "one resolution at a time");
        assert!(!gains.finish_resolve(Some(&sink), 1, first, Some(100)));
        assert_eq!(sink.take(), ["set 100 1 false"]);

        let (_, stale) = gains.begin_resolve(1, false).unwrap();
        assert!(gains.begin_resolve(1, true).is_none());
        assert!(gains.finish_resolve(Some(&sink), 1, stale, Some(100)), "run again");
        assert_eq!(sink.take(), Vec::<String>::new());
        let (_, fresh) = gains.begin_resolve(1, false).unwrap();
        assert!(!gains.finish_resolve(Some(&sink), 1, fresh, Some(300)));
        assert_eq!(sink.take(), ["clear 100", "set 300 1 false"]);

        let (_, failed) = gains.begin_resolve(1, false).unwrap();
        assert!(!gains.finish_resolve(Some(&sink), 1, failed, None));
        assert_eq!(gains.handles(), [(1, true)], "a failed resolution keeps the SSRC");
    }

    #[test]
    fn rootapp_audio_nothing_resolves_into_an_ended_session() {
        let sink = Recorder::default();
        let mut gains = TrackGains::default();
        gains.set(&sink, 1, 1, 1.0, None, None);
        let (_, generation) = gains.begin_resolve(1, false).unwrap();
        assert!(!gains.finish_resolve(None, 1, generation, Some(100)));
        gains.forget(None, 1);
        assert_eq!(sink.take(), Vec::<String>::new());

        gains.set(&sink, 2, 2, 1.0, None, None);
        let (_, generation) = gains.begin_resolve(2, false).unwrap();
        gains.clear();
        assert!(!gains.finish_resolve(Some(&sink), 2, generation, Some(100)));
        assert!(gains.begin_resolve(2, false).is_none());
        assert_eq!(sink.take(), Vec::<String>::new());
    }
}
