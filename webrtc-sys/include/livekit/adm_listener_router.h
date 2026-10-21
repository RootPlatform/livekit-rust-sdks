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

#include <atomic>
#include <cstdint>
#include <functional>
#include <map>
#include <memory>
#include <optional>
#include <utility>
#include <vector>

#include "absl/functional/any_invocable.h"
#include "absl/functional/function_ref.h"
#include "rtc_base/synchronization/mutex.h"

namespace livekit_ffi {

// CoreAudio property selectors (four-character codes) that AdmProxy tells
// apart in the platform ADM's notifications.
constexpr uint32_t kAdmSelectorDevices = 0x64657623;        // 'dev#'
constexpr uint32_t kAdmSelectorDefaultOutput = 0x644F7574;  // 'dOut'
constexpr uint32_t kAdmSelectorDefaultInput = 0x64496E20;   // 'dIn '
constexpr uint32_t kAdmSelectorStreamFormat = 0x73666D74;   // 'sfmt'

struct AdmListenerAddress {
  uint32_t selector = 0;
  uint32_t scope = 0;
  uint32_t element = 0;

  bool operator==(const AdmListenerAddress& other) const = default;
};

struct AdmListenerKey {
  uint32_t object = 0;
  AdmListenerAddress address;
  const void* proc = nullptr;
  void* client = nullptr;

  bool operator==(const AdmListenerKey& other) const = default;
};

using AdmListenerInvoker =
    void (*)(const AdmListenerKey& key,
             uint32_t object,
             const std::vector<AdmListenerAddress>& addresses);

// One notification for a routed listener: the addresses CoreAudio passed,
// and the listener to run for those of them its router hands on (unless the
// listener was removed by then).
struct AdmListenerNotification {
  uint32_t object = 0;
  std::vector<AdmListenerAddress> addresses;
  absl::AnyInvocable<void(const std::vector<AdmListenerAddress>&) &&> invoke;
};

class AdmListenerRouter
    : public std::enable_shared_from_this<AdmListenerRouter> {
 public:
  // Called with the router's lock held; must not wait.
  using Post = std::function<void(AdmListenerNotification)>;

  explicit AdmListenerRouter(Post post);

  void Detach();
  bool attached() const;

  uint64_t routed() const;
  uint64_t delivered() const;

 private:
  friend class AdmListenerRegistry;

  void Deliver(AdmListenerNotification notification);

  mutable webrtc::Mutex mutex_;
  Post post_ RTC_GUARDED_BY(mutex_);
  std::atomic<bool> attached_{true};
  std::atomic<uint64_t> routed_{0};
  std::atomic<uint64_t> delivered_{0};
};

class AdmListenerRegistry {
 public:
  static AdmListenerRegistry& Get();

  using OsCall = absl::FunctionRef<int32_t(uint64_t id)>;

  std::optional<int32_t> Add(const AdmListenerKey& key,
                             AdmListenerInvoker invoke,
                             OsCall os_add);
  std::optional<int32_t> Remove(const AdmListenerKey& key, OsCall os_remove);
  void Notify(uint64_t id,
              uint32_t object,
              std::vector<AdmListenerAddress> addresses);

  static AdmListenerRouter* SetThreadRouter(AdmListenerRouter* router);

 private:
  struct Registration {
    uint64_t id = 0;
    AdmListenerKey key;
    AdmListenerInvoker invoke = nullptr;
    std::shared_ptr<AdmListenerRouter> router;
    std::atomic<bool> removed{false};
  };

  std::shared_ptr<AdmListenerRouter> RouterForLocked(const AdmListenerKey& key)
      RTC_EXCLUSIVE_LOCKS_REQUIRED(mutex_);

  webrtc::Mutex mutex_;
  uint64_t next_id_ RTC_GUARDED_BY(mutex_) = 1;
  std::map<uint64_t, std::shared_ptr<Registration>> registrations_
      RTC_GUARDED_BY(mutex_);
  std::map<std::pair<const void*, void*>, std::weak_ptr<AdmListenerRouter>>
      identities_ RTC_GUARDED_BY(mutex_);
};

class AdmListenerScope {
 public:
  explicit AdmListenerScope(AdmListenerRouter* router)
      : previous_(AdmListenerRegistry::SetThreadRouter(router)) {}
  ~AdmListenerScope() { AdmListenerRegistry::SetThreadRouter(previous_); }

  AdmListenerScope(const AdmListenerScope&) = delete;
  AdmListenerScope& operator=(const AdmListenerScope&) = delete;

 private:
  AdmListenerRouter* const previous_;
};

}  // namespace livekit_ffi
