/*
 * Copyright 2026 LiveKit, Inc.
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

#include "livekit/audio_thread.h"

#include <atomic>

namespace livekit_ffi {
namespace {

thread_local bool t_audio_thread = false;
std::atomic<uint64_t> g_dropped_audio_thread_logs{0};
std::atomic<uint64_t> g_audio_thread_lock_violations{0};

}  // namespace

void MarkAudioThread() {
  t_audio_thread = true;
}

bool IsAudioThread() {
  return t_audio_thread;
}

void CountDroppedAudioThreadLog() {
  g_dropped_audio_thread_logs.fetch_add(1, std::memory_order_relaxed);
}

uint64_t DroppedAudioThreadLogs() {
  return g_dropped_audio_thread_logs.load(std::memory_order_relaxed);
}

void CountAudioThreadLockViolation() {
  g_audio_thread_lock_violations.fetch_add(1, std::memory_order_relaxed);
}

uint64_t AudioThreadLockViolations() {
  return g_audio_thread_lock_violations.load(std::memory_order_relaxed);
}

}  // namespace livekit_ffi
