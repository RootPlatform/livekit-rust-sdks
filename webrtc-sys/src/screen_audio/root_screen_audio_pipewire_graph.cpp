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

// Test entry points of the Linux backend's graph rules, built on every OS.

#include "livekit/root_screen_audio_pipewire_graph.h"

namespace livekit_ffi {

PipeWireGraphPlan pipewire_graph_plan(rust::Slice<const PipeWireGraphNode> nodes,
                                      rust::Slice<const PipeWireGraphClient> clients,
                                      rust::Slice<const PipeWireGraphPort> ports,
                                      rust::Slice<const PipeWireGraphLink> links,
                                      rust::Slice<const int64_t> own_pids,
                                      uint32_t capture_node) {
  pipewire_graph::Graph graph;
  for (const PipeWireGraphNode& source : nodes) {
    pipewire_graph::Node node;
    node.id = source.id;
    node.media_class = std::string(source.media_class);
    node.pid = source.pid;
    node.client_id = source.client_id;
    node.link_group = std::string(source.link_group);
    graph.nodes.push_back(std::move(node));
  }
  for (const PipeWireGraphClient& source : clients) {
    pipewire_graph::Client client;
    client.id = source.id;
    client.pid = source.pid;
    graph.clients.push_back(std::move(client));
  }
  for (const PipeWireGraphPort& source : ports) {
    pipewire_graph::Port port;
    port.id = source.id;
    port.node_id = source.node_id;
    port.output = source.output;
    port.name = std::string(source.name);
    port.channel = std::string(source.channel);
    graph.ports.push_back(std::move(port));
  }
  for (const PipeWireGraphLink& source : links)
    graph.links.push_back({source.output_port, source.input_port});

  std::set<int64_t> own(own_pids.begin(), own_pids.end());
  pipewire_graph::Plan plan = pipewire_graph::MakePlan(graph, own, capture_node);

  PipeWireGraphPlan result;
  result.have_inputs = plan.have_inputs;
  for (const pipewire_graph::StreamDecision& stream : plan.streams) {
    switch (stream.decision) {
      case pipewire_graph::Decision::kLink:
        result.linked.push_back(stream.node_id);
        break;
      case pipewire_graph::Decision::kOwn:
        result.own.push_back(stream.node_id);
        break;
      case pipewire_graph::Decision::kReemitter:
        result.reemitters.push_back(stream.node_id);
        break;
      case pipewire_graph::Decision::kNoPorts:
        result.no_ports.push_back(stream.node_id);
        break;
    }
  }
  for (const pipewire_graph::Link& link : plan.links) {
    PipeWireGraphLink out;
    out.output_port = link.output_port;
    out.input_port = link.input_port;
    result.links.push_back(out);
  }
  return result;
}

bool pipewire_graph_parse_int(rust::Str text, int64_t& value) {
  return pipewire_graph::ParseInt(std::string_view(text.data(), text.size()), &value);
}

int64_t pipewire_graph_stat_parent(rust::Str stat) {
  return pipewire_graph::ParseStatParent(std::string_view(stat.data(), stat.size()));
}

rust::Vec<int64_t> pipewire_graph_process_tree(int64_t self_pid,
                                               rust::Slice<const int64_t> pids,
                                               rust::Slice<const int64_t> parents) {
  std::vector<std::pair<int64_t, int64_t>> pairs;
  for (size_t i = 0; i < pids.size() && i < parents.size(); ++i)
    pairs.emplace_back(pids[i], parents[i]);
  rust::Vec<int64_t> result;
  for (int64_t pid : pipewire_graph::ProcessTree(self_pid, pairs))
    result.push_back(pid);
  return result;
}

}  // namespace livekit_ffi
