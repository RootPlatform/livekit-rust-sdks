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
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::Duration,
};

use env_logger;
use log::{self, LevelFilter, Log};
use tokio::sync::{mpsc, oneshot};

use crate::{proto, FFI_SERVER};

pub const FLUSH_INTERVAL: Duration = Duration::from_secs(1);
pub const BATCH_SIZE: usize = 32;

/// Read on every `livekit_ffi_initialize`: the most verbose level forwarded while
/// capture_logs is on (off, error, warn, info, debug or trace).
pub const LOG_LEVEL_ENV: &str = "LK_FFI_LOG_LEVEL";

/// The capture level from `LK_FFI_LOG_LEVEL`; warn when it is unset or invalid.
pub fn capture_level_from_env() -> LevelFilter {
    std::env::var(LOG_LEVEL_ENV)
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(LevelFilter::Warn)
}

/// Logger that forward logs to the FfiClient when capture_logs is enabled
/// Otherwise fallback to the env_logger
pub struct FfiLogger {
    async_runtime: tokio::runtime::Handle,
    log_tx: mpsc::UnboundedSender<LogMsg>,
    capture_logs: AtomicBool,
    capture_level: AtomicUsize,
    env_logger: env_logger::Logger,
}

enum LogMsg {
    Log(proto::LogRecord),
    Flush(oneshot::Sender<()>),
}

impl FfiLogger {
    pub fn new(async_runtime: tokio::runtime::Handle) -> Self {
        let (log_tx, log_rx) = mpsc::unbounded_channel();
        async_runtime.spawn(log_forward_task(log_rx));

        let env_logger = env_logger::Builder::from_default_env().build();
        FfiLogger {
            async_runtime,
            log_tx,
            capture_logs: AtomicBool::new(false), // Always false by default to ensure the server
            // is always initialized when using capture_logs
            capture_level: AtomicUsize::new(LevelFilter::Warn as usize),
            env_logger,
        }
    }
}

impl FfiLogger {
    pub fn capture_logs(&self) -> bool {
        self.capture_logs.load(Ordering::Acquire)
    }

    /// Records above `level` are dropped here instead of crossing the FFI while capturing.
    pub fn set_capture_logs(&self, capture: bool, level: LevelFilter) {
        self.capture_level.store(level as usize, Ordering::Release);
        self.capture_logs.store(capture, Ordering::Release);
    }

    pub fn capture_level(&self) -> LevelFilter {
        match self.capture_level.load(Ordering::Acquire) {
            0 => LevelFilter::Off,
            1 => LevelFilter::Error,
            2 => LevelFilter::Warn,
            3 => LevelFilter::Info,
            4 => LevelFilter::Debug,
            _ => LevelFilter::Trace,
        }
    }

    /// The level env_logger (RUST_LOG) lets through, used while not capturing.
    pub fn env_filter(&self) -> LevelFilter {
        self.env_logger.filter()
    }
}

impl Log for FfiLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        if !self.capture_logs() {
            return self.env_logger.enabled(metadata);
        }

        metadata.level() <= self.capture_level()
    }

    fn log(&self, record: &log::Record) {
        if !self.capture_logs() {
            return self.env_logger.log(record);
        }
        if !self.enabled(record.metadata()) {
            return;
        }

        self.log_tx.send(LogMsg::Log(record.into())).unwrap();
    }

    fn flush(&self) {
        if !self.capture_logs() {
            return self.env_logger.flush();
        }

        let (tx, rx) = oneshot::channel();
        self.log_tx.send(LogMsg::Flush(tx)).unwrap();
        let _ = self.async_runtime.block_on(rx); // should we block?
    }
}

async fn log_forward_task(mut rx: mpsc::UnboundedReceiver<LogMsg>) {
    async fn flush(batch: &mut Vec<proto::LogRecord>) {
        if batch.is_empty() {
            return;
        }
        // It is safe to use FFI_SERVER here, if we receive logs when capture_logs is enabled,
        // it means the server has already been initialized

        let _ = FFI_SERVER.send_event(
            proto::LogBatch {
            records: batch.clone(), // Avoid clone here?
        }
            .into(),
        );
        batch.clear();
    }

    let mut batch = Vec::with_capacity(BATCH_SIZE);
    let mut interval = tokio::time::interval(FLUSH_INTERVAL);

    loop {
        tokio::select! {
            msg = rx.recv() => {
                if msg.is_none() {
                    break;
                }

                match msg.unwrap() {
                    LogMsg::Log(record) => {
                        batch.push(record);
                    }
                    LogMsg::Flush(tx) => {
                        flush(&mut batch).await;
                        let _ = tx.send(());
                    }
                }
            },
            _ = interval.tick() => {
                flush(&mut batch).await;
            }
        }

        flush(&mut batch).await;
    }

    println!("log forwarding task stopped"); // Shouldn't happen (logger is leaked)
}

impl From<&log::Record<'_>> for proto::LogRecord {
    fn from(record: &log::Record) -> Self {
        proto::LogRecord {
            level: proto::LogLevel::from(record.level()).into(),
            target: record.target().to_string(),
            module_path: record.module_path().map(|s| s.to_string()),
            file: record.file().map(|s| s.to_string()),
            line: record.line(),
            message: record.args().to_string(), // Display trait
        }
    }
}

#[cfg(test)]
mod tests {
    use log::{Level, LevelFilter, Log, Metadata};

    use super::FfiLogger;

    #[test]
    fn capture_forwards_records_up_to_its_level() {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let logger = FfiLogger::new(runtime.handle().clone());
        let enabled =
            |level| logger.enabled(&Metadata::builder().level(level).target("libwebrtc").build());

        logger.set_capture_logs(true, LevelFilter::Warn);
        assert!(enabled(Level::Error) && enabled(Level::Warn));
        assert!(!enabled(Level::Info) && !enabled(Level::Debug) && !enabled(Level::Trace));

        logger.set_capture_logs(true, LevelFilter::Debug);
        assert_eq!(logger.capture_level(), LevelFilter::Debug);
        assert!(enabled(Level::Info) && enabled(Level::Debug));
        assert!(!enabled(Level::Trace));

        logger.set_capture_logs(true, LevelFilter::Off);
        assert!(!enabled(Level::Error));
    }
}

impl From<log::Level> for proto::LogLevel {
    fn from(level: log::Level) -> Self {
        match level {
            log::Level::Error => Self::LogError,
            log::Level::Warn => Self::LogWarn,
            log::Level::Info => Self::LogInfo,
            log::Level::Debug => Self::LogDebug,
            log::Level::Trace => Self::LogTrace,
        }
    }
}
