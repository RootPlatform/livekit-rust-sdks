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

#include <cstddef>
#include <cstdint>
#include <deque>
#include <map>
#include <set>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

// The graph rules of the Linux screen-audio backend
// (src/screen_audio/root_screen_audio_linux.cpp): which application output
// streams its PipeWire capture node links, and how. Free of PipeWire, so every
// OS builds and tests them (src/root_screen_audio_pipewire_graph.rs). A port of
// the desktop's managed PipeWireGraph.DownstreamReemitters, OwnProcessTree and
// PipeWireLoopbackCaptureService.RefreshLinksAsync.

namespace livekit_ffi {
namespace pipewire_graph {

constexpr uint32_t kNoId = 0xffffffffu;
constexpr std::string_view kOutputStream = "Stream/Output/Audio";
constexpr std::string_view kInputStreamPrefix = "Stream/Input";

struct Node {
  uint32_t id = kNoId;
  std::string media_class;      // media.class
  int64_t pid = -1;             // application.process.id, -1 when absent
  uint32_t client_id = kNoId;   // client.id
  std::string link_group;       // node.link-group
  std::string name;             // node.name
  std::string app_name;         // application.name
};

struct Client {
  uint32_t id = kNoId;
  int64_t pid = -1;  // application.process.id
  std::string app_name;
};

struct Port {
  uint32_t id = kNoId;
  uint32_t node_id = kNoId;
  bool output = false;
  std::string name;     // port.name
  std::string channel;  // audio.channel
};

struct Link {
  uint32_t output_port = kNoId;
  uint32_t input_port = kNoId;

  friend bool operator<(const Link& a, const Link& b) {
    return a.output_port != b.output_port ? a.output_port < b.output_port
                                          : a.input_port < b.input_port;
  }
  friend bool operator==(const Link& a, const Link& b) {
    return a.output_port == b.output_port && a.input_port == b.input_port;
  }
};

struct Graph {
  std::vector<Node> nodes;
  std::vector<Client> clients;
  std::vector<Port> ports;
  std::vector<Link> links;
};

enum class Decision : int32_t {
  kLink = 0,       // linked into the capture node
  kOwn = 1,        // a stream of this process tree
  kReemitter = 2,  // re-emits audio of this process tree
  kNoPorts = 3,    // no output ports yet
};

struct StreamDecision {
  uint32_t node_id = kNoId;
  Decision decision = Decision::kNoPorts;
  int64_t pid = -1;
  std::string description;  // application or node name, for logs
  std::vector<Link> links;
};

struct Plan {
  // The capture node has input ports; nothing is planned before it has.
  bool have_inputs = false;
  // Every Stream/Output/Audio node except the capture node, in graph order.
  std::vector<StreamDecision> streams;
  // Every link the capture node should have.
  std::set<Link> links;
};

// Property values: decimal, optional sign and surrounding white space (what
// the managed int.TryParse accepted). `value` is written only on success.
inline bool ParseInt(std::string_view text, int64_t* value) {
  auto space = [](char c) {
    return c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '\v' || c == '\f';
  };
  size_t begin = 0;
  size_t end = text.size();
  while (begin < end && space(text[begin]))
    ++begin;
  while (end > begin && space(text[end - 1]))
    --end;
  bool negative = false;
  if (begin < end && (text[begin] == '+' || text[begin] == '-')) {
    negative = text[begin] == '-';
    ++begin;
  }
  if (begin == end)
    return false;
  constexpr uint64_t kMax = static_cast<uint64_t>(INT64_MAX);
  uint64_t magnitude = 0;
  for (size_t i = begin; i < end; ++i) {
    if (text[i] < '0' || text[i] > '9')
      return false;
    uint64_t digit = static_cast<uint64_t>(text[i] - '0');
    if (magnitude > (kMax - digit) / 10)
      return false;
    magnitude = magnitude * 10 + digit;
  }
  *value = negative ? -static_cast<int64_t>(magnitude) : static_cast<int64_t>(magnitude);
  return true;
}

inline bool ParseId(std::string_view text, uint32_t* value) {
  int64_t parsed = 0;
  if (!ParseInt(text, &parsed) || parsed < 0 || parsed > static_cast<int64_t>(kNoId))
    return false;
  *value = static_cast<uint32_t>(parsed);
  return true;
}

// The parent pid in a /proc/<pid>/stat line ("pid (comm) state ppid ...";
// comm may hold spaces and parentheses, so the fields start after the last
// ')'), or -1.
inline int64_t ParseStatParent(std::string_view stat) {
  size_t close = stat.rfind(')');
  if (close == std::string_view::npos || close + 2 > stat.size())
    return -1;
  std::string_view fields = stat.substr(close + 2);
  size_t first = fields.find(' ');
  if (first == std::string_view::npos)
    return -1;
  std::string_view rest = fields.substr(first + 1);
  size_t second = rest.find(' ');
  int64_t parent = -1;
  if (!ParseInt(second == std::string_view::npos ? rest : rest.substr(0, second), &parent))
    return -1;
  return parent;
}

// `self` and every descendant, from (pid, parent pid) pairs in any order.
inline std::set<int64_t> ProcessTree(int64_t self,
                                     const std::vector<std::pair<int64_t, int64_t>>& parents) {
  std::set<int64_t> own{self};
  bool added = true;
  while (added) {
    added = false;
    for (const auto& [pid, parent] : parents) {
      if (own.count(parent) != 0 && own.insert(pid).second)
        added = true;
    }
  }
  return own;
}

namespace internal {

inline char Lower(char c) {
  return c >= 'A' && c <= 'Z' ? static_cast<char>(c - 'A' + 'a') : c;
}

inline bool EqualsIgnoreCase(std::string_view a, std::string_view b) {
  if (a.size() != b.size())
    return false;
  for (size_t i = 0; i < a.size(); ++i) {
    if (Lower(a[i]) != Lower(b[i]))
      return false;
  }
  return true;
}

inline bool EndsWithIgnoreCase(std::string_view text, std::string_view suffix) {
  return text.size() >= suffix.size() &&
         EqualsIgnoreCase(text.substr(text.size() - suffix.size()), suffix);
}

// The graph by id. A node's pid is its own application.process.id or, when it
// has none, its client's: native libpipewire clients set it on the client only.
struct Index {
  explicit Index(const Graph& graph) {
    std::map<uint32_t, const Client*> clients;
    for (const Client& client : graph.clients)
      clients[client.id] = &client;
    for (const Node& node : graph.nodes) {
      nodes[node.id] = &node;
      int64_t pid = node.pid;
      std::string app = node.app_name;
      auto client = clients.find(node.client_id);
      if (node.client_id != kNoId && client != clients.end()) {
        if (pid < 0)
          pid = client->second->pid;
        if (app.empty())
          app = client->second->app_name;
      }
      pids[node.id] = pid;
      apps[node.id] = app.empty() ? node.name : app;
    }
  }

  int64_t Pid(uint32_t node) const {
    auto it = pids.find(node);
    return it == pids.end() ? -1 : it->second;
  }

  std::map<uint32_t, const Node*> nodes;
  std::map<uint32_t, int64_t> pids;
  std::map<uint32_t, std::string> apps;
};

// What makes two nodes one producer: node.link-group (both halves of a
// loopback or filter-chain, which have no useful pid or client of their own),
// else client.id. Never the pid: daemon-owned nodes all share PipeWire's.
inline std::string ClientKey(const Node& node) {
  if (!node.link_group.empty())
    return "lg:" + node.link_group;
  if (node.client_id != kNoId)
    return "client:" + std::to_string(node.client_id);
  return "node:" + std::to_string(node.id);
}

inline const Port* PickChannel(const std::vector<const Port*>& ports, std::string_view channel) {
  for (const Port* port : ports) {
    if (EqualsIgnoreCase(port->channel, channel))
      return port;
  }
  std::string suffix = "_" + std::string(channel);
  for (const Port* port : ports) {
    if (EndsWithIgnoreCase(port->name, suffix))
      return port;
  }
  return nullptr;
}

}  // namespace internal

// Output streams that carry this process tree's audio although their process
// is another one: effects apps (EasyEffects, JamesDSP), loopbacks and
// filter-chains capture a sink this tree plays into and play the result as a
// stream of their own. From every node of the tree, follow links to what it
// feeds; a capture stream fed directly, or whatever captures a sink it feeds,
// taints its producer (ClientKey), a node with a link group is tainted as it
// is reached, and every output stream of a tainted producer is excluded and
// walked in turn (chained effects). The capture node never counts.
inline std::set<uint32_t> DownstreamReemitters(const Graph& graph,
                                               const std::set<int64_t>& own_tree,
                                               uint32_t capture_node) {
  std::set<uint32_t> excluded;
  if (graph.nodes.empty() || graph.links.empty())
    return excluded;

  internal::Index index(graph);
  std::map<uint32_t, uint32_t> port_node;
  for (const Port& port : graph.ports)
    port_node[port.id] = port.node_id;
  std::map<uint32_t, std::set<uint32_t>> feeds;
  for (const Link& link : graph.links) {
    auto from = port_node.find(link.output_port);
    auto to = port_node.find(link.input_port);
    if (from != port_node.end() && to != port_node.end())
      feeds[from->second].insert(to->second);
  }

  std::set<std::string> tainted;
  std::set<uint32_t> visited;
  std::deque<uint32_t> queue;
  for (const auto& [id, node] : index.nodes) {
    if (id != capture_node && own_tree.count(index.Pid(id)) != 0)
      queue.push_back(id);
  }

  while (!queue.empty()) {
    uint32_t id = queue.front();
    queue.pop_front();
    if (!visited.insert(id).second)
      continue;
    auto targets = feeds.find(id);
    if (targets == feeds.end())
      continue;

    for (uint32_t target : targets->second) {
      auto target_node = index.nodes.find(target);
      if (target == capture_node || target_node == index.nodes.end())
        continue;
      const Node& node = *target_node->second;
      // A filter-chain's sink half hands audio to its output half inside the
      // daemon, without a link; the link group is all that ties them.
      if (!node.link_group.empty())
        tainted.insert(internal::ClientKey(node));
      if (node.media_class.compare(0, kInputStreamPrefix.size(), kInputStreamPrefix) == 0) {
        tainted.insert(internal::ClientKey(node));
      } else if (auto capturers = feeds.find(target); capturers != feeds.end()) {
        for (uint32_t capturer : capturers->second) {
          auto capturer_node = index.nodes.find(capturer);
          if (capturer == capture_node || capturer_node == index.nodes.end())
            continue;
          tainted.insert(internal::ClientKey(*capturer_node->second));
        }
      }
    }

    for (const auto& [node_id, node] : index.nodes) {
      if (node->media_class != kOutputStream || excluded.count(node_id) != 0)
        continue;
      if (own_tree.count(index.Pid(node_id)) != 0)
        continue;
      if (tainted.count(internal::ClientKey(*node)) != 0) {
        excluded.insert(node_id);
        queue.push_back(node_id);
      }
    }
  }
  return excluded;
}

// What the capture node should be linked to: every application output stream
// that is not of this process tree and does not re-emit its audio. Channels:
// FL to FL, FR to FR, MONO to both, other layouts alternately left and right;
// port names ending in _FL/_FR/_MONO stand in for a missing audio.channel.
// The capture node's inputs are FL and FR, else its first and second ports
// (one port takes both).
inline Plan MakePlan(const Graph& graph, const std::set<int64_t>& own_tree, uint32_t capture_node) {
  Plan plan;
  std::vector<const Port*> inputs;
  for (const Port& port : graph.ports) {
    if (port.node_id == capture_node && !port.output)
      inputs.push_back(&port);
  }
  const Port* left = internal::PickChannel(inputs, "FL");
  if (left == nullptr && !inputs.empty())
    left = inputs[0];
  const Port* right = internal::PickChannel(inputs, "FR");
  if (right == nullptr && inputs.size() > 1)
    right = inputs[1];
  if (right == nullptr)
    right = left;
  if (left == nullptr)
    return plan;
  plan.have_inputs = true;

  internal::Index index(graph);
  std::set<uint32_t> reemitters = DownstreamReemitters(graph, own_tree, capture_node);
  for (const Node& node : graph.nodes) {
    if (node.media_class != kOutputStream || node.id == capture_node)
      continue;
    StreamDecision stream;
    stream.node_id = node.id;
    stream.pid = index.Pid(node.id);
    stream.description = index.apps[node.id];
    if (own_tree.count(stream.pid) != 0) {
      stream.decision = Decision::kOwn;
    } else if (reemitters.count(node.id) != 0) {
      stream.decision = Decision::kReemitter;
    } else {
      std::vector<const Port*> outputs;
      for (const Port& port : graph.ports) {
        if (port.node_id == node.id && port.output)
          outputs.push_back(&port);
      }
      if (outputs.empty()) {
        stream.decision = Decision::kNoPorts;
      } else {
        stream.decision = Decision::kLink;
        const Port* source_left = internal::PickChannel(outputs, "FL");
        const Port* source_right = internal::PickChannel(outputs, "FR");
        const Port* source_mono = internal::PickChannel(outputs, "MONO");
        if (source_mono != nullptr) {
          stream.links.push_back({source_mono->id, left->id});
          stream.links.push_back({source_mono->id, right->id});
        } else if (source_left != nullptr || source_right != nullptr) {
          if (source_left != nullptr)
            stream.links.push_back({source_left->id, left->id});
          if (source_right != nullptr)
            stream.links.push_back({source_right->id, right->id});
        } else {
          for (size_t i = 0; i < outputs.size(); ++i)
            stream.links.push_back({outputs[i]->id, (i % 2 == 0 ? left : right)->id});
        }
        plan.links.insert(stream.links.begin(), stream.links.end());
      }
    }
    plan.streams.push_back(std::move(stream));
  }
  return plan;
}

}  // namespace pipewire_graph
}  // namespace livekit_ffi

// Tests (src/root_screen_audio_pipewire_graph.rs): a graph snapshot in, the
// plan out.

#include "rust/cxx.h"
#include "webrtc-sys/src/root_screen_audio_pipewire_graph.rs.h"

namespace livekit_ffi {

PipeWireGraphPlan pipewire_graph_plan(rust::Slice<const PipeWireGraphNode> nodes,
                                      rust::Slice<const PipeWireGraphClient> clients,
                                      rust::Slice<const PipeWireGraphPort> ports,
                                      rust::Slice<const PipeWireGraphLink> links,
                                      rust::Slice<const int64_t> own_pids,
                                      uint32_t capture_node);
bool pipewire_graph_parse_int(rust::Str text, int64_t& value);
int64_t pipewire_graph_stat_parent(rust::Str stat);
rust::Vec<int64_t> pipewire_graph_process_tree(int64_t self_pid,
                                               rust::Slice<const int64_t> pids,
                                               rust::Slice<const int64_t> parents);

}  // namespace livekit_ffi
