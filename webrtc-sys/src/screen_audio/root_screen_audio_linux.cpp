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

// Linux screen audio: a PipeWire capture stream (F32 48 kHz stereo, never
// autoconnected) that this backend links itself, through the registry and the
// link factory, to every application output stream except this process
// tree's and except streams that re-emit its audio (effects apps, loopbacks,
// filter-chains; root_screen_audio_pipewire_graph.h). Streams are linked as
// they appear and unlinked when they turn into re-emitters. No window scoping:
// the portal never names the shared window.
//
// libpipewire-0.3 is loaded at run time: the library links nothing new, loads
// where PipeWire is missing and then reports kUnsupported. Only the exported
// functions come from it; the registry, node and link-factory calls are the
// header's inline method calls through each proxy's interface.
//
// Threads: Start and Stop on the core's control thread; the registry, links and
// stream states on the pw_thread_loop; audio on PipeWire's data loop
// (PW_STREAM_FLAG_RT_PROCESS), where the process callback only dequeues,
// writes into the sink and queues.

#include <dirent.h>
#include <dlfcn.h>
#include <fcntl.h>
#include <unistd.h>

#include <pipewire/pipewire.h>
#include <spa/node/io.h>
#include <spa/param/audio/format-utils.h>

#include <algorithm>
#include <atomic>
#include <cerrno>
#include <chrono>
#include <condition_variable>
#include <cstdio>
#include <cstring>
#include <ctime>
#include <iterator>
#include <map>
#include <memory>
#include <mutex>
#include <set>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "livekit/root_screen_audio_backend.h"
#include "livekit/root_screen_audio_pipewire_graph.h"
#include "rtc_base/logging.h"

namespace livekit_ffi {

namespace {

namespace pwg = pipewire_graph;
using Clock = std::chrono::steady_clock;

constexpr char kLibrary[] = "libpipewire-0.3.so.0";
constexpr int kSampleRate = 48000;
constexpr int kChannels = 2;
constexpr uint32_t kFrameBytes = sizeof(float) * kChannels;
constexpr auto kStartTimeout = std::chrono::seconds(5);
// A link the daemon refused or removed while both ports exist is made again at
// once until it failed this often in a row, then after 1 s and every 5 s
// (until a port is replaced). Failures 10 s apart do not count as in a row.
constexpr int kImmediateLinkRetries = 3;
constexpr auto kFirstLinkBackoff = std::chrono::seconds(1);
constexpr auto kLinkBackoff = std::chrono::seconds(5);
constexpr auto kLinkFailureMemory = std::chrono::seconds(10);

// The exported libpipewire functions this backend calls, resolved with dlsym.
// decltype of the header declarations keeps the signatures exact; nothing here
// references a pw_ symbol at link time.
struct PipeWireApi {
  decltype(&::pw_init) init = nullptr;
  decltype(&::pw_get_library_version) get_library_version = nullptr;
  decltype(&::pw_thread_loop_new) thread_loop_new = nullptr;
  decltype(&::pw_thread_loop_get_loop) thread_loop_get_loop = nullptr;
  decltype(&::pw_thread_loop_start) thread_loop_start = nullptr;
  decltype(&::pw_thread_loop_stop) thread_loop_stop = nullptr;
  decltype(&::pw_thread_loop_destroy) thread_loop_destroy = nullptr;
  decltype(&::pw_thread_loop_lock) thread_loop_lock = nullptr;
  decltype(&::pw_thread_loop_unlock) thread_loop_unlock = nullptr;
  decltype(&::pw_context_new) context_new = nullptr;
  decltype(&::pw_context_destroy) context_destroy = nullptr;
  decltype(&::pw_context_connect) context_connect = nullptr;
  decltype(&::pw_core_disconnect) core_disconnect = nullptr;
  decltype(&::pw_properties_new) properties_new = nullptr;
  decltype(&::pw_stream_new) stream_new = nullptr;
  decltype(&::pw_stream_add_listener) stream_add_listener = nullptr;
  decltype(&::pw_stream_connect) stream_connect = nullptr;
  decltype(&::pw_stream_disconnect) stream_disconnect = nullptr;
  decltype(&::pw_stream_destroy) stream_destroy = nullptr;
  decltype(&::pw_stream_get_node_id) stream_get_node_id = nullptr;
  decltype(&::pw_stream_dequeue_buffer) stream_dequeue_buffer = nullptr;
  decltype(&::pw_stream_queue_buffer) stream_queue_buffer = nullptr;
  decltype(&::pw_proxy_add_listener) proxy_add_listener = nullptr;
  decltype(&::pw_proxy_add_object_listener) proxy_add_object_listener = nullptr;
  decltype(&::pw_proxy_destroy) proxy_destroy = nullptr;
};

template <typename T>
bool Resolve(void* library, const char* name, T* slot) {
  *slot = reinterpret_cast<T>(dlsym(library, name));
  return *slot != nullptr;
}

// Loaded and pw_init'ed once per process; never unloaded.
const PipeWireApi* LoadPipeWire(std::string* error) {
  static std::string failure;
  static const PipeWireApi* const api = []() -> const PipeWireApi* {
    void* library = dlopen(kLibrary, RTLD_NOW | RTLD_LOCAL);
    if (library == nullptr) {
      const char* reason = dlerror();
      failure = std::string(kLibrary) + " is not available" +
                (reason != nullptr ? std::string(" (") + reason + ")" : std::string());
      return nullptr;
    }
    auto* loaded = new PipeWireApi();
    const char* missing = nullptr;
    auto need = [&](const char* name, auto* slot) {
      if (missing == nullptr && !Resolve(library, name, slot))
        missing = name;
    };
    need("pw_init", &loaded->init);
    need("pw_get_library_version", &loaded->get_library_version);
    need("pw_thread_loop_new", &loaded->thread_loop_new);
    need("pw_thread_loop_get_loop", &loaded->thread_loop_get_loop);
    need("pw_thread_loop_start", &loaded->thread_loop_start);
    need("pw_thread_loop_stop", &loaded->thread_loop_stop);
    need("pw_thread_loop_destroy", &loaded->thread_loop_destroy);
    need("pw_thread_loop_lock", &loaded->thread_loop_lock);
    need("pw_thread_loop_unlock", &loaded->thread_loop_unlock);
    need("pw_context_new", &loaded->context_new);
    need("pw_context_destroy", &loaded->context_destroy);
    need("pw_context_connect", &loaded->context_connect);
    need("pw_core_disconnect", &loaded->core_disconnect);
    need("pw_properties_new", &loaded->properties_new);
    need("pw_stream_new", &loaded->stream_new);
    need("pw_stream_add_listener", &loaded->stream_add_listener);
    need("pw_stream_connect", &loaded->stream_connect);
    need("pw_stream_disconnect", &loaded->stream_disconnect);
    need("pw_stream_destroy", &loaded->stream_destroy);
    need("pw_stream_get_node_id", &loaded->stream_get_node_id);
    need("pw_stream_dequeue_buffer", &loaded->stream_dequeue_buffer);
    need("pw_stream_queue_buffer", &loaded->stream_queue_buffer);
    need("pw_proxy_add_listener", &loaded->proxy_add_listener);
    need("pw_proxy_add_object_listener", &loaded->proxy_add_object_listener);
    need("pw_proxy_destroy", &loaded->proxy_destroy);
    if (missing != nullptr) {
      failure = std::string(kLibrary) + " has no " + missing;
      delete loaded;
      return nullptr;
    }
    loaded->init(nullptr, nullptr);
    return loaded;
  }();
  if (api == nullptr)
    *error = failure;
  return api;
}

std::string ErrnoText(int error) {
  char text[128];
  std::snprintf(text, sizeof(text), "%s, errno %d", std::strerror(error), error);
  return text;
}

std::string_view Lookup(const spa_dict* props, const char* key) {
  const char* value = props != nullptr ? spa_dict_lookup(props, key) : nullptr;
  return value != nullptr ? std::string_view(value) : std::string_view();
}

pwg::Node ReadNode(uint32_t id, const spa_dict* props) {
  pwg::Node node;
  node.id = id;
  node.media_class = std::string(Lookup(props, "media.class"));
  pwg::ParseInt(Lookup(props, "application.process.id"), &node.pid);
  pwg::ParseId(Lookup(props, "client.id"), &node.client_id);
  node.link_group = std::string(Lookup(props, "node.link-group"));
  node.name = std::string(Lookup(props, "node.name"));
  node.app_name = std::string(Lookup(props, "application.name"));
  node.device = !Lookup(props, "device.id").empty();
  return node;
}

pwg::Client ReadClient(uint32_t id, const spa_dict* props) {
  pwg::Client client;
  client.id = id;
  pwg::ParseInt(Lookup(props, "application.process.id"), &client.pid);
  client.app_name = std::string(Lookup(props, "application.name"));
  return client;
}

// This process and its descendants (DotNetBrowser's Chromium and anything
// else Root starts), from /proc/<pid>/stat.
std::set<int64_t> OwnProcessTree() {
  std::vector<std::pair<int64_t, int64_t>> parents;
  if (DIR* proc = opendir("/proc")) {
    while (dirent* entry = readdir(proc)) {
      int64_t pid = 0;
      if (entry->d_name[0] < '0' || entry->d_name[0] > '9' || !pwg::ParseInt(entry->d_name, &pid))
        continue;
      char path[64];
      std::snprintf(path, sizeof(path), "/proc/%s/stat", entry->d_name);
      int fd = open(path, O_RDONLY | O_CLOEXEC);
      if (fd < 0)
        continue;
      char text[512];
      ssize_t length = read(fd, text, sizeof(text));
      close(fd);
      if (length <= 0)
        continue;
      int64_t parent = pwg::ParseStatParent(std::string_view(text, static_cast<size_t>(length)));
      if (parent >= 0)
        parents.emplace_back(pid, parent);
    }
    closedir(proc);
  }
  return pwg::ProcessTree(getpid(), parents);
}

class PipeWireScreenAudioBackend final : public ScreenAudioBackend {
 public:
  PipeWireScreenAudioBackend() {
    core_events_.version = PW_VERSION_CORE_EVENTS;
    core_events_.done = &OnCoreDone;
    core_events_.error = &OnCoreError;
    registry_events_.version = PW_VERSION_REGISTRY_EVENTS;
    registry_events_.global = &OnGlobal;
    registry_events_.global_remove = &OnGlobalRemove;
    stream_events_.version = PW_VERSION_STREAM_EVENTS;
    stream_events_.state_changed = &OnStreamState;
    stream_events_.io_changed = &OnStreamIo;
    stream_events_.param_changed = &OnStreamParam;
    stream_events_.process = &OnProcess;
    node_events_.version = PW_VERSION_NODE_EVENTS;
    node_events_.info = &OnNodeInfo;
    client_events_.version = PW_VERSION_CLIENT_EVENTS;
    client_events_.info = &OnClientInfo;
    link_events_.version = PW_VERSION_PROXY_EVENTS;
    link_events_.removed = &OnLinkRemoved;
    link_events_.error = &OnLinkError;
  }

  ~PipeWireScreenAudioBackend() override { Stop(); }

  ScreenAudioStartResult Start(const ScreenAudioTarget& target,
                               ScreenAudioSink* sink) override {
    Stop();
    ScreenAudioStartResult result;
    std::string failure;
    api_ = LoadPipeWire(&failure);
    if (api_ == nullptr) {
      result.error = ScreenAudioError::kUnsupported;
      result.detail = failure;
      return result;
    }
    sink_ = sink;
    {
      std::lock_guard<std::mutex> lock(mutex_);
      phase_ = Phase::kStarting;
      ready_ = false;
      start_failed_ = false;
      start_error_ = ScreenAudioError::kNone;
      start_detail_.clear();
      reported_ = false;
      // A restart may follow a daemon restart, whose graph can lack the
      // devices for a while: only a first start checks it.
      audio_checked_ = target.restart;
      handles_audio_ = true;
    }
    cycle_valid_ = false;
    writing_.store(true, std::memory_order_release);

    ScreenAudioError error = Open(&failure);
    if (error == ScreenAudioError::kNone)
      error = WaitUntilReady(&failure);
    if (error != ScreenAudioError::kNone) {
      Stop();
      result.error = error;
      result.detail = failure;
      return result;
    }

    result.format = ScreenAudioFormat{kSampleRate, kChannels};
    result.scope = ScreenAudioScope::kSystem;
    result.detail = "all audio except process " + std::to_string(getpid()) + " and its children";
    if (target.kind != ScreenAudioTargetKind::kSystem)
      result.detail = "window scoping is not available on Linux; " + result.detail;
    const char* version = api_->get_library_version();
    result.detail += std::string(" (PipeWire ") + (version != nullptr ? version : "?") + ")";
    return result;
  }

  bool Stop() override {
    {
      std::lock_guard<std::mutex> lock(mutex_);
      if (phase_ == Phase::kIdle)
        return true;
      phase_ = Phase::kStopping;
    }
    Teardown();
    std::lock_guard<std::mutex> lock(mutex_);
    phase_ = Phase::kIdle;
    return true;
  }

 private:
  enum class Phase { kIdle, kStarting, kRunning, kStopping };

  // A bound node or client: its info carries the full properties (the
  // registry's global properties lack application.process.id and
  // node.link-group).
  struct Bound {
    PipeWireScreenAudioBackend* owner = nullptr;
    pw_proxy* proxy = nullptr;
    spa_hook listener{};
    bool listening = false;
    bool has_info = false;
    uint64_t serial = 0;
  };
  struct NodeEntry : Bound {
    pwg::Node node;
  };
  struct ClientEntry : Bound {
    pwg::Client client;
  };
  // A link this backend made. Removed or refused ones are released on the next
  // pass, never inside their own proxy event.
  struct LinkEntry {
    PipeWireScreenAudioBackend* owner = nullptr;
    pwg::Link link;
    pw_proxy* proxy = nullptr;
    spa_hook listener{};
    bool listening = false;
    bool finished = false;
    bool refused = false;
  };

  // Control thread. Creates the loop, connects and starts the stream.
  ScreenAudioError Open(std::string* failure) {
    loop_ = api_->thread_loop_new("root-screen-audio", nullptr);
    if (loop_ == nullptr) {
      *failure = "pw_thread_loop_new failed";
      return ScreenAudioError::kInternal;
    }
    main_loop_ = api_->thread_loop_get_loop(loop_);
    context_ = api_->context_new(main_loop_, nullptr, 0);
    if (context_ == nullptr) {
      *failure = "pw_context_new failed (" + ErrnoText(errno) + ")";
      return ScreenAudioError::kStartFailed;
    }
    if (api_->thread_loop_start(loop_) < 0) {
      *failure = "pw_thread_loop_start failed";
      return ScreenAudioError::kInternal;
    }
    api_->thread_loop_lock(loop_);
    ScreenAudioError error = Connect(failure);
    api_->thread_loop_unlock(loop_);
    return error;
  }

  // Under the loop lock.
  ScreenAudioError Connect(std::string* failure) {
    core_ = api_->context_connect(context_, nullptr, 0);
    if (core_ == nullptr) {
      int error = errno;
      *failure = "cannot connect to the PipeWire daemon (" + ErrnoText(error) + ")";
      return error == EACCES || error == EPERM ? ScreenAudioError::kPermissionDenied
                                               : ScreenAudioError::kStartFailed;
    }
    (void)pw_core_add_listener(core_, &core_listener_, &core_events_, this);
    core_listening_ = true;
    registry_ = pw_core_get_registry(core_, PW_VERSION_REGISTRY, 0);
    if (registry_ == nullptr) {
      *failure = "pw_core_get_registry failed";
      return ScreenAudioError::kStartFailed;
    }
    (void)pw_registry_add_listener(registry_, &registry_listener_, &registry_events_, this);
    registry_listening_ = true;
    // Answered once the daemon listed every global.
    registry_sync_seq_ = pw_core_sync(core_, PW_ID_CORE, 0);
    registry_sync_pending_ = registry_sync_seq_ >= 0;
    if (!registry_sync_pending_) {
      std::lock_guard<std::mutex> lock(mutex_);
      audio_checked_ = true;
    }
    retry_timer_ = pw_loop_add_timer(main_loop_, &OnRetryTimer, this);

    // Never autoconnected: without node.autoconnect=false the session manager
    // links the stream to the default microphone. The links are ours.
    pw_properties* props = api_->properties_new(
        "media.type", "Audio", "media.category", "Capture", "media.role", "Screen", "node.name",
        "root-screen-share-audio", "node.description", "Root screen share audio",
        "application.name", "Root", "node.autoconnect", "false", "node.dont-reconnect", "true",
        nullptr);
    stream_ = api_->stream_new(core_, "Root screen share audio", props);
    if (stream_ == nullptr) {
      *failure = "pw_stream_new failed (" + ErrnoText(errno) + ")";
      return ScreenAudioError::kStartFailed;
    }
    api_->stream_add_listener(stream_, &stream_listener_, &stream_events_, this);
    stream_listening_ = true;

    uint8_t buffer[1024];
    spa_pod_builder builder{};
    builder.data = buffer;
    builder.size = sizeof(buffer);
    spa_audio_info_raw info{};
    info.format = SPA_AUDIO_FORMAT_F32_LE;
    info.rate = kSampleRate;
    info.channels = kChannels;
    info.position[0] = SPA_AUDIO_CHANNEL_FL;
    info.position[1] = SPA_AUDIO_CHANNEL_FR;
    const spa_pod* params[1] = {spa_format_audio_raw_build(&builder, SPA_PARAM_EnumFormat, &info)};
    int res = api_->stream_connect(
        stream_, PW_DIRECTION_INPUT, PW_ID_ANY,
        static_cast<pw_stream_flags>(PW_STREAM_FLAG_MAP_BUFFERS | PW_STREAM_FLAG_RT_PROCESS), params,
        1);
    if (res < 0) {
      *failure = "pw_stream_connect failed (" + ErrnoText(-res) + ")";
      return ScreenAudioError::kStartFailed;
    }
    return ScreenAudioError::kNone;
  }

  // Control thread: until the stream has its node (paused) and the graph
  // was listed, or the start failed.
  ScreenAudioError WaitUntilReady(std::string* failure) {
    std::unique_lock<std::mutex> lock(mutex_);
    bool settled = changed_.wait_for(lock, kStartTimeout, [this] {
      return (ready_ && audio_checked_) || start_failed_;
    });
    if (start_failed_) {
      *failure = start_detail_;
      return start_error_;
    }
    if (!settled) {
      *failure = "the PipeWire stream did not start within 5 s";
      return ScreenAudioError::kTimeout;
    }
    if (!handles_audio_) {
      *failure =
          "PipeWire does not handle this system's audio (its graph has no audio sink and no "
          "playback stream; the sound server is probably PulseAudio without pipewire-pulse), so "
          "there is nothing to capture";
      return ScreenAudioError::kUnsupported;
    }
    phase_ = Phase::kRunning;
    return ScreenAudioError::kNone;
  }

  // Control thread. The stream leaves the data loop in pw_stream_disconnect, so
  // once it returned no process callback runs.
  void Teardown() {
    writing_.store(false, std::memory_order_release);
    if (loop_ != nullptr) {
      api_->thread_loop_lock(loop_);
      if (stream_ != nullptr) {
        api_->stream_disconnect(stream_);
        if (stream_listening_)
          spa_hook_remove(&stream_listener_);
        stream_listening_ = false;
        api_->stream_destroy(stream_);
        stream_ = nullptr;
      }
      position_.store(nullptr, std::memory_order_release);
      if (retry_timer_ != nullptr) {
        pw_loop_destroy_source(main_loop_, retry_timer_);
        retry_timer_ = nullptr;
      }
      for (auto& [link, entry] : own_links_)
        Release(&entry->proxy, &entry->listener, &entry->listening);
      own_links_.clear();
      for (auto& [id, entry] : nodes_)
        Release(&entry->proxy, &entry->listener, &entry->listening);
      nodes_.clear();
      for (auto& [id, entry] : clients_)
        Release(&entry->proxy, &entry->listener, &entry->listening);
      clients_.clear();
      if (registry_ != nullptr) {
        if (registry_listening_)
          spa_hook_remove(&registry_listener_);
        registry_listening_ = false;
        api_->proxy_destroy(reinterpret_cast<pw_proxy*>(registry_));
        registry_ = nullptr;
      }
      if (core_listening_)
        spa_hook_remove(&core_listener_);
      core_listening_ = false;
      api_->thread_loop_unlock(loop_);
      api_->thread_loop_stop(loop_);
    }
    if (core_ != nullptr) {
      api_->core_disconnect(core_);
      core_ = nullptr;
    }
    if (context_ != nullptr) {
      api_->context_destroy(context_);
      context_ = nullptr;
    }
    if (loop_ != nullptr) {
      api_->thread_loop_destroy(loop_);
      loop_ = nullptr;
    }
    main_loop_ = nullptr;
    ports_.clear();
    links_.clear();
    link_failures_.clear();
    decisions_.clear();
    capture_node_ = PW_ID_ANY;
    dirty_ = false;
    sync_pending_ = false;
    registry_sync_pending_ = false;
    bind_serial_ = 0;
    sync_request_serial_ = 0;
    synced_serial_ = 0;
    sink_ = nullptr;
  }

  void Release(pw_proxy** proxy, spa_hook* listener, bool* listening) {
    if (*listening)
      spa_hook_remove(listener);
    *listening = false;
    if (*proxy != nullptr)
      api_->proxy_destroy(*proxy);
    *proxy = nullptr;
  }

  // Loop thread. A fatal failure while starting fails the start; a running
  // stream reports it once and goes quiet (the core stops it and decides on a
  // restart). One that is not fatal is only logged.
  void Fail(ScreenAudioError error, const std::string& detail, bool fatal = true) {
    bool report = false;
    {
      std::lock_guard<std::mutex> lock(mutex_);
      if (!fatal) {
        if (phase_ == Phase::kStarting || phase_ == Phase::kRunning)
          RTC_LOG(LS_WARNING) << "Screen audio (PipeWire): " << detail;
      } else if (phase_ == Phase::kStarting) {
        if (!start_failed_) {
          start_failed_ = true;
          start_error_ =
              error == ScreenAudioError::kDeviceLost || error == ScreenAudioError::kStreamFailed
                  ? ScreenAudioError::kStartFailed
                  : error;
          start_detail_ = detail;
        }
        changed_.notify_all();
      } else if (phase_ == Phase::kRunning && !reported_) {
        reported_ = true;
        report = true;
      }
    }
    if (!report)
      return;
    writing_.store(false, std::memory_order_release);
    RTC_LOG(LS_WARNING) << "Screen audio (PipeWire): " << detail;
    sink_->ReportError(error, detail.c_str());
  }

  // Graph changes are coalesced with a core sync: the plan runs once the
  // daemon answered everything asked before it, bound objects' info included.
  void MarkDirty() {
    dirty_ = true;
    if (sync_pending_ || core_ == nullptr)
      return;
    sync_request_serial_ = bind_serial_;
    int seq = pw_core_sync(core_, PW_ID_CORE, 0);
    if (seq < 0)
      return;
    sync_seq_ = seq;
    sync_pending_ = true;
  }

  // An object bound after the last answered sync whose info has not arrived.
  bool AwaitingInfo() const {
    for (const auto& [id, entry] : nodes_) {
      if (!entry->has_info && entry->proxy != nullptr && entry->serial > synced_serial_)
        return true;
    }
    for (const auto& [id, entry] : clients_) {
      if (!entry->has_info && entry->proxy != nullptr && entry->serial > synced_serial_)
        return true;
    }
    return false;
  }

  void NoteLinkFailure(const pwg::Link& link) {
    LinkFailures& failures = link_failures_[link];
    const Clock::time_point now = Clock::now();
    if (failures.count > 0 && now - failures.last >= kLinkFailureMemory)
      failures.count = 0;
    ++failures.count;
    failures.last = now;
    if (failures.count < kImmediateLinkRetries) {
      failures.retry_at = now;
      return;
    }
    if (failures.count == kImmediateLinkRetries) {
      RTC_LOG(LS_WARNING) << "Screen audio (PipeWire): link " << link.output_port << " -> "
                          << link.input_port << " failed " << kImmediateLinkRetries
                          << " times in a row; retrying after 1 s, then every 5 s";
    }
    failures.retry_at =
        now + (failures.count == kImmediateLinkRetries ? kFirstLinkBackoff : kLinkBackoff);
  }

  // The retry timer fires when the earliest backed-off link the plan still
  // wants is due.
  void ArmRetry(const pwg::Plan& plan, const std::set<pwg::Link>& existing,
                Clock::time_point now) {
    if (retry_timer_ == nullptr)
      return;
    bool due = false;
    Clock::time_point earliest{};
    for (const auto& [link, failures] : link_failures_) {
      if (failures.retry_at <= now || plan.links.count(link) == 0 ||
          own_links_.count(link) != 0 || existing.count(link) != 0)
        continue;
      if (!due || failures.retry_at < earliest)
        earliest = failures.retry_at;
      due = true;
    }
    if (!due) {
      pw_loop_update_timer(main_loop_, retry_timer_, nullptr, nullptr, false);
      return;
    }
    int64_t wait = std::max<int64_t>(
        1, std::chrono::duration_cast<std::chrono::nanoseconds>(earliest - now).count());
    timespec value{};
    value.tv_sec = static_cast<time_t>(wait / 1000000000);
    value.tv_nsec = static_cast<long>(wait % 1000000000);
    pw_loop_update_timer(main_loop_, retry_timer_, &value, nullptr, false);
  }

  void Reconcile() {
    for (auto it = own_links_.begin(); it != own_links_.end();) {
      LinkEntry* entry = it->second.get();
      if (!entry->finished) {
        ++it;
        continue;
      }
      const pwg::Link link = entry->link;
      if (entry->refused ||
          (ports_.count(link.output_port) != 0 && ports_.count(link.input_port) != 0))
        NoteLinkFailure(link);
      Release(&entry->proxy, &entry->listener, &entry->listening);
      it = own_links_.erase(it);
    }
    if (capture_node_ == PW_ID_ANY)
      return;
    if (AwaitingInfo()) {
      MarkDirty();
      return;
    }

    pwg::Graph graph;
    for (const auto& [id, entry] : nodes_)
      graph.nodes.push_back(entry->node);
    for (const auto& [id, entry] : clients_)
      graph.clients.push_back(entry->client);
    for (const auto& [id, port] : ports_)
      graph.ports.push_back(port);
    for (const auto& [id, link] : links_)
      graph.links.push_back(link);
    pwg::Plan plan = pwg::MakePlan(graph, OwnProcessTree(), capture_node_);
    if (!plan.have_inputs)
      return;
    LogDecisions(plan);

    for (auto it = own_links_.begin(); it != own_links_.end();) {
      if (plan.links.count(it->first) != 0) {
        ++it;
        continue;
      }
      Release(&it->second->proxy, &it->second->listener, &it->second->listening);
      it = own_links_.erase(it);
    }
    std::set<pwg::Link> existing(graph.links.begin(), graph.links.end());
    const Clock::time_point now = Clock::now();
    for (const pwg::Link& link : plan.links) {
      if (own_links_.count(link) != 0 || existing.count(link) != 0)
        continue;
      auto failures = link_failures_.find(link);
      if (failures != link_failures_.end() && now < failures->second.retry_at)
        continue;
      CreateLink(link);
    }
    ArmRetry(plan, existing, now);
  }

  void CreateLink(const pwg::Link& link) {
    auto port = ports_.find(link.output_port);
    if (port == ports_.end())
      return;
    // The link factory needs both node ids, not just the ports. Without
    // object.linger the link belongs to this client and dies with it.
    std::string output_node = std::to_string(port->second.node_id);
    std::string output_port = std::to_string(link.output_port);
    std::string input_node = std::to_string(capture_node_);
    std::string input_port = std::to_string(link.input_port);
    spa_dict_item items[] = {
        {"link.output.node", output_node.c_str()}, {"link.output.port", output_port.c_str()},
        {"link.input.node", input_node.c_str()},   {"link.input.port", input_port.c_str()},
        {"object.linger", "false"},
    };
    spa_dict dict{};
    dict.n_items = static_cast<uint32_t>(std::size(items));
    dict.items = items;
    void* proxy = pw_core_create_object(core_, "link-factory", PW_TYPE_INTERFACE_Link,
                                        PW_VERSION_LINK, &dict, 0);
    if (proxy == nullptr) {
      NoteLinkFailure(link);
      return;
    }
    auto entry = std::make_unique<LinkEntry>();
    entry->owner = this;
    entry->link = link;
    entry->proxy = static_cast<pw_proxy*>(proxy);
    api_->proxy_add_listener(entry->proxy, &entry->listener, &link_events_, entry.get());
    entry->listening = true;
    own_links_[link] = std::move(entry);
  }

  static std::string Describe(const pwg::StreamDecision& stream) {
    std::string text = stream.description.empty() ? std::string("a stream")
                                                   : "'" + stream.description + "'";
    text += " (";
    if (stream.pid >= 0)
      text += "pid " + std::to_string(stream.pid) + ", ";
    return text + "node " + std::to_string(stream.node_id) + ")";
  }

  // Each node's decision once, again when it changes.
  void LogDecisions(const pwg::Plan& plan) {
    for (const pwg::StreamDecision& stream : plan.streams) {
      if (stream.decision == pwg::Decision::kNoPorts)
        continue;
      auto [it, inserted] = decisions_.try_emplace(stream.node_id, stream.decision);
      if (!inserted && it->second == stream.decision)
        continue;
      it->second = stream.decision;
      switch (stream.decision) {
        case pwg::Decision::kLink:
          RTC_LOG(LS_INFO) << "Screen audio (PipeWire): linking " << Describe(stream);
          break;
        case pwg::Decision::kReemitter:
          RTC_LOG(LS_INFO) << "Screen audio (PipeWire): not linking " << Describe(stream)
                           << ", it re-emits audio of this process (an effects app, loopback "
                              "or filter-chain downstream of it)";
          break;
        case pwg::Decision::kOwn:
          RTC_LOG(LS_VERBOSE) << "Screen audio (PipeWire): not linking this process tree's "
                              << Describe(stream);
          break;
        case pwg::Decision::kNoPorts:
          break;
      }
    }
  }

  template <typename Entry>
  void Bind(Entry* entry, uint32_t id, const char* type, uint32_t version, uint32_t ours,
            const void* events) {
    entry->owner = this;
    entry->serial = ++bind_serial_;
    void* proxy = pw_registry_bind(registry_, id, type, std::min(version, ours), 0);
    if (proxy == nullptr)
      return;
    entry->proxy = static_cast<pw_proxy*>(proxy);
    api_->proxy_add_object_listener(entry->proxy, &entry->listener, events, entry);
    entry->listening = true;
  }

  bool RemoveNode(uint32_t id) {
    auto it = nodes_.find(id);
    if (it == nodes_.end())
      return false;
    Release(&it->second->proxy, &it->second->listener, &it->second->listening);
    nodes_.erase(it);
    decisions_.erase(id);
    return true;
  }

  bool RemoveClient(uint32_t id) {
    auto it = clients_.find(id);
    if (it == clients_.end())
      return false;
    Release(&it->second->proxy, &it->second->listener, &it->second->listening);
    clients_.erase(it);
    return true;
  }

  void AddGlobal(uint32_t id, const char* type, uint32_t version, const spa_dict* props) {
    if (std::strcmp(type, PW_TYPE_INTERFACE_Node) == 0) {
      RemoveNode(id);
      auto entry = std::make_unique<NodeEntry>();
      entry->node = ReadNode(id, props);
      NodeEntry* bound = entry.get();
      nodes_[id] = std::move(entry);
      Bind(bound, id, PW_TYPE_INTERFACE_Node, version, PW_VERSION_NODE, &node_events_);
    } else if (std::strcmp(type, PW_TYPE_INTERFACE_Client) == 0) {
      RemoveClient(id);
      auto entry = std::make_unique<ClientEntry>();
      entry->client = ReadClient(id, props);
      ClientEntry* bound = entry.get();
      clients_[id] = std::move(entry);
      Bind(bound, id, PW_TYPE_INTERFACE_Client, version, PW_VERSION_CLIENT, &client_events_);
    } else if (std::strcmp(type, PW_TYPE_INTERFACE_Port) == 0) {
      pwg::Port port;
      port.id = id;
      if (!pwg::ParseId(Lookup(props, "node.id"), &port.node_id))
        return;
      std::string_view direction = Lookup(props, "port.direction");
      port.output = direction == "out" || direction == "output";
      port.name = std::string(Lookup(props, "port.name"));
      port.channel = std::string(Lookup(props, "audio.channel"));
      ports_[id] = std::move(port);
    } else if (std::strcmp(type, PW_TYPE_INTERFACE_Link) == 0) {
      pwg::Link link;
      if (!pwg::ParseId(Lookup(props, "link.output.port"), &link.output_port) ||
          !pwg::ParseId(Lookup(props, "link.input.port"), &link.input_port))
        return;
      links_[id] = link;
    } else {
      return;
    }
    MarkDirty();
  }

  void RemoveGlobal(uint32_t id) {
    bool changed = RemoveNode(id);
    changed = RemoveClient(id) || changed;
    if (ports_.erase(id) != 0) {
      for (auto it = link_failures_.begin(); it != link_failures_.end();) {
        if (it->first.output_port == id || it->first.input_port == id)
          it = link_failures_.erase(it);
        else
          ++it;
      }
      changed = true;
    }
    changed = links_.erase(id) != 0 || changed;
    if (changed)
      MarkDirty();
  }

  void OnReady() {
    if (capture_node_ == PW_ID_ANY) {
      capture_node_ = api_->stream_get_node_id(stream_);
      if (capture_node_ != PW_ID_ANY)
        MarkDirty();
    }
    std::lock_guard<std::mutex> lock(mutex_);
    if (phase_ == Phase::kStarting && capture_node_ != PW_ID_ANY) {
      ready_ = true;
      changed_.notify_all();
    }
  }

  static PipeWireScreenAudioBackend* Self(void* data) {
    return static_cast<PipeWireScreenAudioBackend*>(data);
  }

  // The first start only runs where PipeWire plays the audio: with PulseAudio
  // as the sound server (PipeWire for video only) the capture would stay
  // silent.
  void OnRegistryListed() {
    std::vector<pwg::Node> nodes;
    for (const auto& [id, entry] : nodes_)
      nodes.push_back(entry->node);
    const bool handles_audio = pwg::HandlesAudio(nodes);
    std::lock_guard<std::mutex> lock(mutex_);
    if (audio_checked_)
      return;
    audio_checked_ = true;
    handles_audio_ = handles_audio;
    changed_.notify_all();
  }

  static void OnRetryTimer(void* data, uint64_t expirations) {
    (void)expirations;
    Self(data)->MarkDirty();
  }

  static void OnCoreDone(void* data, uint32_t id, int seq) {
    PipeWireScreenAudioBackend* self = Self(data);
    if (id != PW_ID_CORE)
      return;
    if (self->registry_sync_pending_ && seq == self->registry_sync_seq_) {
      self->registry_sync_pending_ = false;
      self->OnRegistryListed();
    }
    if (!self->sync_pending_ || seq != self->sync_seq_)
      return;
    self->sync_pending_ = false;
    self->synced_serial_ = self->sync_request_serial_;
    if (self->dirty_) {
      self->dirty_ = false;
      self->Reconcile();
    }
  }

  static void OnCoreError(void* data, uint32_t id, int seq, int res, const char* message) {
    (void)seq;
    // Errors of the links this backend made arrive on their proxies.
    if (id != PW_ID_CORE)
      return;
    std::string detail = std::string("PipeWire error: ") +
                         (message != nullptr ? message : "unknown") + " (" + ErrnoText(-res) + ")";
    if (res == -EPIPE)
      Self(data)->Fail(ScreenAudioError::kDeviceLost, "the PipeWire daemon closed the connection");
    else if (res == -EACCES || res == -EPERM)
      Self(data)->Fail(ScreenAudioError::kPermissionDenied, detail);
    else
      Self(data)->Fail(ScreenAudioError::kStartFailed, detail, /*fatal=*/false);
  }

  static void OnGlobal(void* data,
                       uint32_t id,
                       uint32_t permissions,
                       const char* type,
                       uint32_t version,
                       const spa_dict* props) {
    (void)permissions;
    if (type != nullptr)
      Self(data)->AddGlobal(id, type, version, props);
  }

  static void OnGlobalRemove(void* data, uint32_t id) { Self(data)->RemoveGlobal(id); }

  static void OnNodeInfo(void* data, const pw_node_info* info) {
    auto* entry = static_cast<NodeEntry*>(data);
    if (info == nullptr || (entry->has_info && (info->change_mask & PW_NODE_CHANGE_MASK_PROPS) == 0))
      return;
    if (info->props != nullptr)
      entry->node = ReadNode(entry->node.id, info->props);
    entry->has_info = true;
    entry->owner->MarkDirty();
  }

  static void OnClientInfo(void* data, const pw_client_info* info) {
    auto* entry = static_cast<ClientEntry*>(data);
    if (info == nullptr ||
        (entry->has_info && (info->change_mask & PW_CLIENT_CHANGE_MASK_PROPS) == 0))
      return;
    if (info->props != nullptr)
      entry->client = ReadClient(entry->client.id, info->props);
    entry->has_info = true;
    entry->owner->MarkDirty();
  }

  static void OnLinkRemoved(void* data) {
    auto* entry = static_cast<LinkEntry*>(data);
    entry->finished = true;
    entry->owner->MarkDirty();
  }

  static void OnLinkError(void* data, int seq, int res, const char* message) {
    (void)seq;
    auto* entry = static_cast<LinkEntry*>(data);
    if (!entry->finished && entry->owner->link_failures_.count(entry->link) == 0) {
      RTC_LOG(LS_WARNING) << "Screen audio (PipeWire): link " << entry->link.output_port << " -> "
                          << entry->link.input_port << " refused: "
                          << (message != nullptr ? message : "") << " (" << ErrnoText(-res) << ")";
    }
    entry->finished = true;
    entry->refused = true;
    entry->owner->MarkDirty();
  }

  static void OnStreamState(void* data,
                            pw_stream_state old_state,
                            pw_stream_state state,
                            const char* error) {
    PipeWireScreenAudioBackend* self = Self(data);
    switch (state) {
      case PW_STREAM_STATE_PAUSED:
      case PW_STREAM_STATE_STREAMING:
        self->OnReady();
        break;
      case PW_STREAM_STATE_ERROR:
        self->Fail(ScreenAudioError::kDeviceLost,
                   std::string("PipeWire stream error: ") + (error != nullptr ? error : "unknown"));
        break;
      case PW_STREAM_STATE_UNCONNECTED:
        if (old_state != PW_STREAM_STATE_UNCONNECTED)
          self->Fail(ScreenAudioError::kDeviceLost, "the PipeWire stream was disconnected");
        break;
      default:
        break;
    }
  }

  // The format is fixed (one EnumFormat), so this only guards against a daemon
  // that negotiated something else.
  static void OnStreamParam(void* data, uint32_t id, const spa_pod* param) {
    if (id != SPA_PARAM_Format || param == nullptr)
      return;
    uint32_t media_type = 0;
    uint32_t media_subtype = 0;
    if (spa_format_parse(param, &media_type, &media_subtype) < 0 ||
        media_type != SPA_MEDIA_TYPE_audio || media_subtype != SPA_MEDIA_SUBTYPE_raw)
      return;
    spa_audio_info_raw info{};
    if (spa_format_audio_raw_parse(param, &info) < 0)
      return;
    if (info.format == SPA_AUDIO_FORMAT_F32_LE && info.rate == static_cast<uint32_t>(kSampleRate) &&
        info.channels == static_cast<uint32_t>(kChannels))
      return;
    Self(data)->Fail(ScreenAudioError::kStreamFailed,
                     "PipeWire negotiated " + std::to_string(info.rate) + " Hz, " +
                         std::to_string(info.channels) + " channels, format " +
                         std::to_string(static_cast<uint32_t>(info.format)) +
                         " instead of 48 kHz stereo float");
  }

  static void OnStreamIo(void* data, uint32_t id, void* area, uint32_t size) {
    if (id != SPA_IO_Position)
      return;
    const spa_io_position* position =
        area != nullptr && size >= sizeof(spa_io_clock) ? static_cast<spa_io_position*>(area)
                                                        : nullptr;
    Self(data)->position_.store(position, std::memory_order_release);
  }

  // Data loop. A cycle that starts after the end of the previous one this
  // node ran in means it missed cycles (an xrun).
  void NoteCycle(const spa_io_clock& clock) {
    const uint64_t position = clock.position;
    const uint64_t duration = clock.duration;
    const uint32_t id = clock.id;
    const uint32_t rate = clock.rate.denom;
    if (cycle_valid_ && id == cycle_clock_ && rate == cycle_rate_ && position > cycle_next_)
      sink_->MarkDiscontinuity();
    cycle_next_ = position + duration;
    cycle_clock_ = id;
    cycle_rate_ = rate;
    cycle_valid_ = true;
  }

  // Data loop, real time: no allocation, locks, logging or syscalls.
  static void OnProcess(void* data) {
    PipeWireScreenAudioBackend* self = Self(data);
    const PipeWireApi* api = self->api_;
    pw_stream* stream = self->stream_;
    const bool writing = self->writing_.load(std::memory_order_acquire);
    const spa_io_position* position = self->position_.load(std::memory_order_acquire);
    if (writing && position != nullptr)
      self->NoteCycle(position->clock);
    while (pw_buffer* buffer = api->stream_dequeue_buffer(stream)) {
      spa_buffer* spa = buffer->buffer;
      if (writing && spa != nullptr && spa->n_datas > 0) {
        const spa_data& plane = spa->datas[0];
        if (plane.data != nullptr && plane.chunk != nullptr && plane.maxsize > 0) {
          uint32_t offset = plane.chunk->offset % plane.maxsize;
          uint32_t size = std::min(plane.chunk->size, plane.maxsize - offset);
          size_t frames = size / kFrameBytes;
          if (frames > 0)
            self->sink_->Write(
                reinterpret_cast<const float*>(static_cast<const uint8_t*>(plane.data) + offset),
                frames);
        }
      }
      api->stream_queue_buffer(stream, buffer);
    }
  }

  struct LinkFailures {
    int count = 0;
    Clock::time_point last{};
    Clock::time_point retry_at{};
  };

  const PipeWireApi* api_ = nullptr;
  ScreenAudioSink* sink_ = nullptr;
  std::atomic<bool> writing_{false};
  // The driver's position (io_changed, any thread; read on the data loop).
  std::atomic<const spa_io_position*> position_{nullptr};

  // Data loop: the cycle this node expects next.
  bool cycle_valid_ = false;
  uint64_t cycle_next_ = 0;
  uint32_t cycle_clock_ = 0;
  uint32_t cycle_rate_ = 0;

  pw_core_events core_events_{};
  pw_registry_events registry_events_{};
  pw_stream_events stream_events_{};
  pw_node_events node_events_{};
  pw_client_events client_events_{};
  pw_proxy_events link_events_{};

  // Control thread, and the loop thread under the loop lock.
  pw_thread_loop* loop_ = nullptr;
  pw_loop* main_loop_ = nullptr;
  spa_source* retry_timer_ = nullptr;
  pw_context* context_ = nullptr;
  pw_core* core_ = nullptr;
  pw_registry* registry_ = nullptr;
  pw_stream* stream_ = nullptr;
  spa_hook core_listener_{};
  spa_hook registry_listener_{};
  spa_hook stream_listener_{};
  bool core_listening_ = false;
  bool registry_listening_ = false;
  bool stream_listening_ = false;

  // The start handshake and the error report (loop thread and control thread).
  std::mutex mutex_;
  std::condition_variable changed_;
  Phase phase_ = Phase::kIdle;
  bool ready_ = false;
  bool start_failed_ = false;
  ScreenAudioError start_error_ = ScreenAudioError::kNone;
  std::string start_detail_;
  bool reported_ = false;
  // Whether the graph was listed (or need not be) and shows PipeWire playing
  // the system's audio.
  bool audio_checked_ = false;
  bool handles_audio_ = true;

  // Loop thread.
  uint32_t capture_node_ = PW_ID_ANY;
  std::map<uint32_t, std::unique_ptr<NodeEntry>> nodes_;
  std::map<uint32_t, std::unique_ptr<ClientEntry>> clients_;
  std::map<uint32_t, pwg::Port> ports_;
  std::map<uint32_t, pwg::Link> links_;
  std::map<pwg::Link, std::unique_ptr<LinkEntry>> own_links_;
  std::map<pwg::Link, LinkFailures> link_failures_;
  std::map<uint32_t, pwg::Decision> decisions_;
  bool dirty_ = false;
  bool sync_pending_ = false;
  int sync_seq_ = 0;
  bool registry_sync_pending_ = false;
  int registry_sync_seq_ = 0;
  uint64_t bind_serial_ = 0;
  uint64_t sync_request_serial_ = 0;
  uint64_t synced_serial_ = 0;
};

}  // namespace

bool RootScreenAudioBackendBuilt() {
  return true;
}

std::unique_ptr<ScreenAudioBackend> CreateRootScreenAudioBackend() {
  return std::make_unique<PipeWireScreenAudioBackend>();
}

}  // namespace livekit_ffi
