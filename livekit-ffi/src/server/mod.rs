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
    error::Error,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};

use dashmap::{mapref::one::MappedRef, DashMap};
use downcast_rs::{impl_downcast, Downcast};
use livekit::prelude::DisconnectReason;
use livekit::webrtc::{
    native::apm::AudioProcessingModule, native::audio_resampler::AudioResampler, prelude::*,
};
use parking_lot::{deadlock, Mutex};
use tokio::{sync::oneshot, task::JoinHandle};

use crate::{proto, proto::FfiEvent, FfiError, FfiHandleId, FfiResult, INVALID_HANDLE};

pub mod audio_plugin;
pub mod audio_source;
pub mod audio_stream;
pub mod colorcvt;
pub mod data_stream;
pub mod data_track;
pub mod logger;
pub mod participant;
pub mod platform_audio;
pub mod requests;
pub mod resampler;
pub mod room;
mod utils;
pub mod video_source;
pub mod video_stream;

//#[cfg(test)]
//mod tests;

#[cfg(test)]
mod audio_filter_tests;

#[derive(Clone)]
pub struct FfiConfig {
    pub callback_fn: Arc<dyn Fn(FfiEvent) + Send + Sync>,
    pub capture_logs: bool,
    pub sdk: String,
    pub sdk_version: String,
}

/// To make sure we use the right types, only types that implement this trait
/// can be stored inside the FfiServer.
pub trait FfiHandle: Downcast + Send + Sync {}
impl_downcast!(FfiHandle);

#[derive(Clone)]
pub struct FfiDataBuffer {
    pub handle: FfiHandleId,
    pub data: Arc<Vec<u8>>,
}

impl FfiHandle for FfiDataBuffer {}
impl FfiHandle for Arc<Mutex<AudioResampler>> {}
impl FfiHandle for Arc<Mutex<AudioProcessingModule>> {}
impl FfiHandle for Arc<Mutex<resampler::SoxResampler>> {}
impl FfiHandle for AudioFrame<'static> {}
impl FfiHandle for BoxVideoBuffer {}
impl FfiHandle for Box<[u8]> {}
impl FfiHandle for () {}

pub struct FfiServer {
    /// Store all Ffi handles inside an HashMap, if this isn't efficient enough
    /// We can still use Box::into_raw & Box::from_raw in the future (but keep it safe for now)
    ffi_handles: DashMap<FfiHandleId, Box<dyn FfiHandle>>,
    pub async_runtime: tokio::runtime::Runtime,
    /// Dedicated runtime for audio capture to reduce scheduling delays
    pub audio_runtime: tokio::runtime::Runtime,

    next_id: AtomicU64,
    config: Mutex<Option<FfiConfig>>,
    logger: &'static logger::FfiLogger,
    handle_dropped_txs: DashMap<FfiHandleId, Vec<oneshot::Sender<()>>>,
}

const DEFAULT_MAX_WORKER_THREADS: usize = 8;
const WORKER_THREADS_ENV: &str = "LK_FFI_WORKER_THREADS";

fn resolve_worker_threads(available: usize, requested: Option<&str>) -> usize {
    let available = available.max(1);
    match requested.and_then(|value| value.trim().parse::<usize>().ok()) {
        Some(requested) if requested > 0 => requested.min(available),
        _ => available.min(DEFAULT_MAX_WORKER_THREADS),
    }
}

fn async_worker_threads() -> usize {
    let available = thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    resolve_worker_threads(available, std::env::var(WORKER_THREADS_ENV).ok().as_deref())
}

fn build_async_runtime(worker_threads: usize) -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .thread_name("tokio-rt-worker")
        .enable_all()
        .build()
        .unwrap()
}

impl Default for FfiServer {
    fn default() -> Self {
        let async_runtime = build_async_runtime(async_worker_threads());

        // Dedicated single-threaded runtime for audio capture
        // This ensures audio tasks never compete with other operations
        let audio_runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("livekit-audio")
            .enable_all()
            .build()
            .unwrap();

        let logger = Box::leak(Box::new(logger::FfiLogger::new(async_runtime.handle().clone())));
        log::set_logger(logger).unwrap();
        log::set_max_level(logger.env_filter());

        #[cfg(feature = "tracing")]
        console_subscriber::init();

        // Create a background thread which checks for deadlocks every 10s
        thread::spawn(move || loop {
            thread::sleep(Duration::from_secs(10));
            let deadlocks = deadlock::check_deadlock();
            if deadlocks.is_empty() {
                continue;
            }

            log::error!("{} deadlocks detected", deadlocks.len());
            for (i, threads) in deadlocks.iter().enumerate() {
                log::error!("Deadlock #{}", i);
                for t in threads {
                    log::error!("Thread Id {:#?}: \n{:#?}", t.thread_id(), t.backtrace());
                }
            }
        });

        Self {
            ffi_handles: Default::default(),
            next_id: AtomicU64::new(1), // 0 is invalid
            async_runtime,
            audio_runtime,
            config: Default::default(),
            logger,
            handle_dropped_txs: Default::default(),
        }
    }
}

// Using &'static self inside the implementation, not sure if this is really idiomatic
// It simplifies the code a lot tho. In most cases the server is used until the end of the process
impl FfiServer {
    pub fn setup(&self, config: FfiConfig) {
        *self.config.lock() = Some(config.clone());
        let level = if config.capture_logs {
            logger::capture_level_from_env()
        } else {
            self.logger.env_filter()
        };
        self.set_log_level(config.capture_logs, level);

        log::debug!(
            "initializing ffi server v{} ({} async workers)",
            env!("CARGO_PKG_VERSION"),
            self.async_runtime.metrics().num_workers()
        ); // TODO: Move this log
    }

    // The log macros and libwebrtc's own log calls skip records above `level`, so nothing is
    // formatted only to be dropped by the logger.
    fn set_log_level(&self, capture: bool, level: log::LevelFilter) {
        self.logger.set_capture_logs(capture, level);
        log::set_max_level(level);
        livekit::webrtc::native::set_log_level(level);
    }

    /// Returns whether the server has been setup.
    pub fn is_setup(&self) -> bool {
        self.config.lock().is_some()
    }

    /// Snapshot of all currently-stored rooms.
    pub fn list_rooms(&self) -> Vec<room::FfiRoom> {
        self.ffi_handles
            .iter()
            .filter_map(|h| h.value().downcast_ref::<room::FfiRoom>().cloned())
            .collect()
    }

    pub async fn dispose(&'static self) {
        log::debug!("disposing ffi server");

        // Close all rooms
        let rooms = self.list_rooms();

        for room in rooms {
            room.close(self, DisconnectReason::ClientInitiated).await;
        }

        self.set_log_level(false, self.logger.env_filter());

        // Drop all handles
        *self.config.lock() = None; // Invalidate the config
    }

    pub fn send_event(&self, message: proto::ffi_event::Message) -> FfiResult<()> {
        let cb = self
            .config
            .lock()
            .as_ref()
            .map_or_else(|| Err(FfiError::NotConfigured), |c| Ok(c.callback_fn.clone()))?;

        cb(proto::FfiEvent { message: Some(message) });
        Ok(())
    }

    pub fn next_id(&self) -> FfiHandleId {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Resolves the async_id to use for a request.
    /// Uses the client-provided ID if available, otherwise generates a new one.
    pub fn resolve_async_id(&self, request_async_id: Option<u64>) -> FfiHandleId {
        request_async_id.unwrap_or_else(|| self.next_id())
    }

    pub fn store_handle<T>(&self, id: FfiHandleId, handle: T)
    where
        T: FfiHandle,
    {
        self.ffi_handles.insert(id, Box::new(handle));
    }

    pub fn retrieve_handle<T>(
        &self,
        id: FfiHandleId,
    ) -> FfiResult<MappedRef<'_, u64, Box<dyn FfiHandle>, T>>
    where
        T: FfiHandle,
    {
        if id == INVALID_HANDLE {
            return Err(FfiError::InvalidRequest("handle is invalid".into()));
        }

        let handle =
            self.ffi_handles.get(&id).ok_or(FfiError::InvalidRequest("handle not found".into()))?;

        if !handle.is::<T>() {
            let tyname = std::any::type_name::<T>();
            let msg = format!("handle is not a {}", tyname);
            return Err(FfiError::InvalidRequest(msg.into()));
        }

        let handle = handle.map(|v| v.downcast_ref::<T>().unwrap());
        Ok(handle)
    }

    pub fn take_handle<T>(&self, id: FfiHandleId) -> FfiResult<T>
    where
        T: FfiHandle,
    {
        if id == INVALID_HANDLE {
            return Err(FfiError::InvalidRequest("handle is invalid".into()));
        }

        let (_, handle) = self
            .ffi_handles
            .remove(&id)
            .ok_or(FfiError::InvalidRequest("handle not found".into()))?;

        let handle = handle.downcast::<T>().map_err(|_| {
            let tyname = std::any::type_name::<T>();
            let msg = format!("handle is not a {}", tyname);
            FfiError::InvalidRequest(msg.into())
        })?;
        Ok(*handle)
    }

    pub fn drop_handle(&self, id: FfiHandleId) -> bool {
        let existed = self.ffi_handles.remove(&id).is_some();
        self.handle_dropped_txs.remove(&id);
        if !existed {
            log::warn!("Attempted to drop unknown FFI handle: {id}");
        }
        return existed;
    }

    pub fn watch_handle_dropped(&self, handle: FfiHandleId) -> oneshot::Receiver<()> {
        // Create vec if not exists
        if self.handle_dropped_txs.get(&handle).is_none() {
            self.handle_dropped_txs.insert(handle, Vec::new());
        }
        let (tx, rx) = oneshot::channel::<()>();
        let mut tx_vec = self.handle_dropped_txs.get_mut(&handle).unwrap();
        tx_vec.push(tx);
        return rx;
    }

    pub fn send_panic(&self, err: Box<dyn Error>) {
        let _ = self.send_event(proto::Panic { message: err.as_ref().to_string() }.into());
    }

    pub fn watch_panic<O>(&'static self, handle: JoinHandle<O>) -> JoinHandle<O>
    where
        O: Send + 'static,
    {
        let handle = self.async_runtime.spawn(async move {
            match handle.await {
                Ok(r) => r,
                Err(e) => {
                    // Forward the panic to the client
                    // Recommended behaviour is to exit the process
                    log::error!("task panicked: {:?}", e);
                    self.send_panic(Box::new(e));
                    panic!("watch_panic: task panicked");
                }
            }
        });
        handle
    }
}

#[cfg(test)]
mod runtime_tests {
    use super::{async_worker_threads, build_async_runtime, resolve_worker_threads};
    use crate::FFI_SERVER;

    #[test]
    fn default_caps_workers_at_eight() {
        assert_eq!(resolve_worker_threads(32, None), 8);
        assert_eq!(resolve_worker_threads(16, None), 8);
        assert_eq!(resolve_worker_threads(8, None), 8);
        assert_eq!(resolve_worker_threads(4, None), 4);
        assert_eq!(resolve_worker_threads(2, None), 2);
        assert_eq!(resolve_worker_threads(1, None), 1);
        assert_eq!(resolve_worker_threads(0, None), 1);
    }

    #[test]
    fn override_is_bounded_by_available_parallelism() {
        assert_eq!(resolve_worker_threads(32, Some("1")), 1);
        assert_eq!(resolve_worker_threads(32, Some(" 8 ")), 8);
        assert_eq!(resolve_worker_threads(32, Some("32")), 32);
        assert_eq!(resolve_worker_threads(32, Some("64")), 32);
        assert_eq!(resolve_worker_threads(2, Some("4")), 2);
    }

    #[test]
    fn invalid_override_falls_back_to_default() {
        for value in ["", "0", "-2", "four", "2.5"] {
            assert_eq!(resolve_worker_threads(32, Some(value)), 8, "override {value:?}");
        }
    }

    #[test]
    fn async_runtime_runs_the_requested_named_workers() {
        let runtime = build_async_runtime(3);
        assert_eq!(runtime.metrics().num_workers(), 3);
        let name = runtime.block_on(async {
            tokio::spawn(async { std::thread::current().name().map(str::to_owned) }).await.unwrap()
        });
        assert_eq!(name.as_deref(), Some("tokio-rt-worker"));
    }

    #[test]
    fn ffi_server_uses_the_resolved_worker_count() {
        assert_eq!(FFI_SERVER.async_runtime.metrics().num_workers(), async_worker_threads());
        assert_eq!(FFI_SERVER.audio_runtime.metrics().num_workers(), 1);
    }
}
