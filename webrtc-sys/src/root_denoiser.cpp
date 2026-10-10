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

#include "livekit/root_denoiser.h"

namespace livekit_ffi {

RootDenoiserSlot::RootDenoiserSlot() = default;
RootDenoiserSlot::~RootDenoiserSlot() = default;

RootDenoiseResult RootDenoiserSlot::ProcessAudio(const float*,
                                                 size_t,
                                                 int,
                                                 bool,
                                                 float,
                                                 float*,
                                                 size_t) {
  return {};
}

void RootDenoiserSlot::LatchBypass(RootDenoiserBypass) {}

bool RootDenoiserSlot::Install(RootFxPipeline*,
                               uint32_t,
                               int,
                               RootFxPipeline** previous) {
  if (previous) {
    *previous = nullptr;
  }
  return false;
}

bool RootDenoiserSlot::Remove(int, RootFxPipeline** previous) {
  if (previous) {
    *previous = nullptr;
  }
  return true;
}

bool RootDenoiserSlot::installed() const {
  return false;
}

uint32_t RootDenoiserSlot::rate() const {
  return 0;
}

RootDenoiserBypass RootDenoiserSlot::bypass() const {
  return RootDenoiserBypass::kNone;
}

RootDenoiserSlotStats RootDenoiserSlot::ReadStats(bool) {
  return {};
}

bool RootDenoiserSlot::InstallForTest(void*, uint32_t) {
  return false;
}

void* RootDenoiserSlot::RemoveForTest() {
  return nullptr;
}

RootFxNotificationHandler* RootDenoiserNotificationHandler() {
  return nullptr;
}

RootDenoiserNotificationCounts root_denoiser_notification_counts() {
  return {};
}

size_t root_denoiser_notification_handler() {
  return 0;
}

bool root_denoiser_supported() {
  return false;
}

const RootFxPipelineVtbl RootFakeFxPipeline::kVtbl = {};

RootFakeFxPipeline::RootFakeFxPipeline(const RootFakeFxConfig& config)
    : object_{&kVtbl, this}, config_(config), sleep_us_(config.sleep_us) {}

RootFakeFxPipeline::~RootFakeFxPipeline() = default;

void* RootFakeFxPipeline::pipeline() {
  return &object_;
}

void RootFakeFxPipeline::set_sleep_us(uint32_t sleep_us) {
  sleep_us_.store(sleep_us, std::memory_order_relaxed);
}

void RootFakeFxPipeline::set_empty(bool empty) {
  empty_.store(empty, std::memory_order_relaxed);
}

void RootFakeFxPipeline::Poison() {
  poisoned_.store(true, std::memory_order_relaxed);
}

uint64_t RootFakeFxPipeline::process_calls() const {
  return process_calls_.load(std::memory_order_relaxed);
}

uint64_t RootFakeFxPipeline::flush_calls() const {
  return flush_calls_.load(std::memory_order_relaxed);
}

uint64_t RootFakeFxPipeline::power_sets() const {
  return power_sets_.load(std::memory_order_relaxed);
}

float RootFakeFxPipeline::last_power() const {
  return power_.load(std::memory_order_relaxed);
}

uint64_t RootFakeFxPipeline::calls_after_poison() const {
  return calls_after_poison_.load(std::memory_order_relaxed);
}

uint64_t RootFakeFxPipeline::releases() const {
  return releases_.load(std::memory_order_relaxed);
}

}  // namespace livekit_ffi
