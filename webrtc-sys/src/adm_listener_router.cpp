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

#include "livekit/adm_listener_router.h"

#include <utility>

#if defined(WEBRTC_MAC) && !defined(WEBRTC_IOS)
#include <CoreAudio/CoreAudio.h>
#include <dlfcn.h>

#include "rtc_base/logging.h"
#endif

namespace livekit_ffi {
namespace {

thread_local AdmListenerRouter* t_router = nullptr;

}  // namespace

AdmListenerRouter::AdmListenerRouter(Post post) : post_(std::move(post)) {}

void AdmListenerRouter::Detach() {
  webrtc::MutexLock lock(&mutex_);
  attached_.store(false, std::memory_order_release);
  post_ = nullptr;
}

bool AdmListenerRouter::attached() const {
  return attached_.load(std::memory_order_acquire);
}

uint64_t AdmListenerRouter::routed() const {
  return routed_.load(std::memory_order_relaxed);
}

uint64_t AdmListenerRouter::delivered() const {
  return delivered_.load(std::memory_order_relaxed);
}

void AdmListenerRouter::Deliver(AdmListenerNotification notification) {
  webrtc::MutexLock lock(&mutex_);
  if (!post_) {
    return;
  }
  delivered_.fetch_add(1, std::memory_order_relaxed);
  post_(std::move(notification));
}

AdmListenerRegistry& AdmListenerRegistry::Get() {
  static AdmListenerRegistry* const registry = new AdmListenerRegistry();
  return *registry;
}

AdmListenerRouter* AdmListenerRegistry::SetThreadRouter(
    AdmListenerRouter* router) {
  return std::exchange(t_router, router);
}

std::shared_ptr<AdmListenerRouter> AdmListenerRegistry::RouterForLocked(
    const AdmListenerKey& key) {
  std::shared_ptr<AdmListenerRouter> router;
  if (t_router != nullptr) {
    router = t_router->weak_from_this().lock();
  } else {
    const auto found = identities_.find({key.proc, key.client});
    if (found != identities_.end()) {
      router = found->second.lock();
      if (!router) {
        identities_.erase(found);
      }
    }
  }
  if (router && !router->attached()) {
    return nullptr;
  }
  return router;
}

std::optional<int32_t> AdmListenerRegistry::Add(const AdmListenerKey& key,
                                                AdmListenerInvoker invoke,
                                                OsCall os_add) {
  std::shared_ptr<Registration> registration;
  {
    webrtc::MutexLock lock(&mutex_);
    std::shared_ptr<AdmListenerRouter> router = RouterForLocked(key);
    if (!router) {
      return std::nullopt;
    }
    for (auto it = registrations_.begin(); it != registrations_.end(); ++it) {
      if (it->second->key == key) {
        if (it->second->router == router) {
          return 0;
        }
        it->second->removed.store(true, std::memory_order_release);
        registrations_.erase(it);
        break;
      }
    }
    registration = std::make_shared<Registration>();
    registration->id = next_id_++;
    registration->key = key;
    registration->invoke = invoke;
    registration->router = router;
    registrations_[registration->id] = registration;
    identities_[{key.proc, key.client}] = router;
  }
  const int32_t status = os_add(registration->id);
  if (status != 0) {
    registration->removed.store(true, std::memory_order_release);
    webrtc::MutexLock lock(&mutex_);
    registrations_.erase(registration->id);
    return status;
  }
  registration->router->routed_.fetch_add(1, std::memory_order_relaxed);
  return status;
}

std::optional<int32_t> AdmListenerRegistry::Remove(const AdmListenerKey& key,
                                                   OsCall os_remove) {
  uint64_t id = 0;
  {
    webrtc::MutexLock lock(&mutex_);
    for (auto it = registrations_.begin(); it != registrations_.end(); ++it) {
      if (it->second->key == key) {
        it->second->removed.store(true, std::memory_order_release);
        id = it->first;
        registrations_.erase(it);
        break;
      }
    }
  }
  if (id == 0) {
    return std::nullopt;
  }
  return os_remove(id);
}

void AdmListenerRegistry::Notify(uint64_t id,
                                 uint32_t object,
                                 std::vector<AdmListenerAddress> addresses) {
  std::shared_ptr<Registration> registration;
  {
    webrtc::MutexLock lock(&mutex_);
    const auto found = registrations_.find(id);
    if (found == registrations_.end()) {
      return;
    }
    registration = found->second;
  }
  AdmListenerNotification notification;
  notification.object = object;
  notification.addresses = std::move(addresses);
  notification.invoke =
      [registration, object](const std::vector<AdmListenerAddress>& handed) {
        if (!registration->removed.load(std::memory_order_acquire)) {
          registration->invoke(registration->key, object, handed);
        }
      };
  registration->router->Deliver(std::move(notification));
}

#if defined(WEBRTC_MAC) && !defined(WEBRTC_IOS)
namespace {

using ListenerCall = OSStatus (*)(AudioObjectID,
                                  const AudioObjectPropertyAddress*,
                                  AudioObjectPropertyListenerProc,
                                  void*);

ListenerCall CoreAudioCall(const char* name) {
  static void* const core_audio =
      dlopen("/System/Library/Frameworks/CoreAudio.framework/CoreAudio",
             RTLD_LAZY | RTLD_LOCAL);
  ListenerCall call =
      core_audio != nullptr
          ? reinterpret_cast<ListenerCall>(dlsym(core_audio, name))
          : nullptr;
  if (call == nullptr) {
    RTC_LOG(LS_ERROR) << "AdmListenerRouter: CoreAudio's " << name
                      << " not found";
  }
  return call;
}

ListenerCall OsAdd() {
  static const ListenerCall call =
      CoreAudioCall("AudioObjectAddPropertyListener");
  return call;
}

ListenerCall OsRemove() {
  static const ListenerCall call =
      CoreAudioCall("AudioObjectRemovePropertyListener");
  return call;
}

AdmListenerKey KeyOf(AudioObjectID object,
                     const AudioObjectPropertyAddress& address,
                     AudioObjectPropertyListenerProc proc,
                     void* client) {
  AdmListenerKey key;
  key.object = object;
  key.address = {address.mSelector, address.mScope, address.mElement};
  key.proc = reinterpret_cast<const void*>(proc);
  key.client = client;
  return key;
}

void InvokeCoreAudioListener(const AdmListenerKey& key,
                             uint32_t object,
                             const std::vector<AdmListenerAddress>& addresses) {
  std::vector<AudioObjectPropertyAddress> native;
  native.reserve(addresses.size());
  for (const AdmListenerAddress& address : addresses) {
    native.push_back({address.selector, address.scope, address.element});
  }
  const auto proc = reinterpret_cast<AudioObjectPropertyListenerProc>(
      const_cast<void*>(key.proc));
  proc(object, static_cast<UInt32>(native.size()), native.data(), key.client);
}

OSStatus RoutedListener(AudioObjectID object,
                        UInt32 count,
                        const AudioObjectPropertyAddress* addresses,
                        void* client) {
  std::vector<AdmListenerAddress> copy;
  copy.reserve(count);
  for (UInt32 i = 0; i < count; ++i) {
    copy.push_back(
        {addresses[i].mSelector, addresses[i].mScope, addresses[i].mElement});
  }
  AdmListenerRegistry::Get().Notify(reinterpret_cast<uintptr_t>(client), object,
                                    std::move(copy));
  return noErr;
}

void* RoutedClient(uint64_t id) {
  return reinterpret_cast<void*>(static_cast<uintptr_t>(id));
}

}  // namespace
#endif

}  // namespace livekit_ffi

#if defined(WEBRTC_MAC) && !defined(WEBRTC_IOS)
extern "C" OSStatus AudioObjectAddPropertyListener(
    AudioObjectID inObjectID,
    const AudioObjectPropertyAddress* inAddress,
    AudioObjectPropertyListenerProc inListener,
    void* inClientData) {
  using namespace livekit_ffi;
  const ListenerCall os_add = OsAdd();
  if (os_add == nullptr) {
    return kAudioHardwareUnspecifiedError;
  }
  if (inAddress != nullptr && inListener != nullptr) {
    const std::optional<int32_t> routed = AdmListenerRegistry::Get().Add(
        KeyOf(inObjectID, *inAddress, inListener, inClientData),
        InvokeCoreAudioListener, [&](uint64_t id) {
          return os_add(inObjectID, inAddress, RoutedListener,
                        RoutedClient(id));
        });
    if (routed) {
      return *routed;
    }
  }
  return os_add(inObjectID, inAddress, inListener, inClientData);
}

extern "C" OSStatus AudioObjectRemovePropertyListener(
    AudioObjectID inObjectID,
    const AudioObjectPropertyAddress* inAddress,
    AudioObjectPropertyListenerProc inListener,
    void* inClientData) {
  using namespace livekit_ffi;
  const ListenerCall os_remove = OsRemove();
  if (os_remove == nullptr) {
    return kAudioHardwareUnspecifiedError;
  }
  if (inAddress != nullptr && inListener != nullptr) {
    const std::optional<int32_t> routed = AdmListenerRegistry::Get().Remove(
        KeyOf(inObjectID, *inAddress, inListener, inClientData),
        [&](uint64_t id) {
          return os_remove(inObjectID, inAddress, RoutedListener,
                           RoutedClient(id));
        });
    if (routed) {
      return *routed;
    }
  }
  return os_remove(inObjectID, inAddress, inListener, inClientData);
}
#endif
