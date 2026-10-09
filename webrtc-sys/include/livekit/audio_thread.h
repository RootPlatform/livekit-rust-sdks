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

#pragma once

#include <cstdint>

namespace livekit_ffi {

// Device capture and render threads mark themselves on their first callback and
// stay marked for their lifetime. Log lines raised on them are dropped (and
// counted) instead of crossing into Rust, and debug builds assert that no
// AdmProxy lock is taken on them.
void MarkAudioThread();
bool IsAudioThread();

void CountDroppedAudioThreadLog();
uint64_t DroppedAudioThreadLogs();

}  // namespace livekit_ffi
