// Copyright 2026 LiveKit, Inc.
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

//! The graph rules of the Linux screen-audio backend
//! (`root_screen_audio_pipewire_graph.h`): which application output streams its
//! PipeWire capture node links, and how. Free of PipeWire, so every OS tests
//! them: a graph snapshot in, the link decisions out.

#[cxx::bridge(namespace = "livekit_ffi")]
pub mod ffi {
    #[derive(Debug, Clone, Default)]
    pub struct PipeWireGraphNode {
        pub id: u32,
        pub media_class: String,
        /// application.process.id; -1 when the node has none.
        pub pid: i64,
        /// client.id; u32::MAX when the node has none.
        pub client_id: u32,
        /// node.link-group
        pub link_group: String,
    }

    #[derive(Debug, Clone, Copy, Default)]
    pub struct PipeWireGraphClient {
        pub id: u32,
        /// application.process.id; -1 when the client has none.
        pub pid: i64,
    }

    #[derive(Debug, Clone, Default)]
    pub struct PipeWireGraphPort {
        pub id: u32,
        pub node_id: u32,
        pub output: bool,
        pub name: String,
        pub channel: String,
    }

    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct PipeWireGraphLink {
        pub output_port: u32,
        pub input_port: u32,
    }

    /// Output stream nodes by decision, and the capture node's links.
    #[derive(Debug, Clone, Default)]
    pub struct PipeWireGraphPlan {
        pub have_inputs: bool,
        pub linked: Vec<u32>,
        pub own: Vec<u32>,
        pub reemitters: Vec<u32>,
        pub no_ports: Vec<u32>,
        pub links: Vec<PipeWireGraphLink>,
    }

    unsafe extern "C++" {
        include!("livekit/root_screen_audio_pipewire_graph.h");

        fn pipewire_graph_plan(
            nodes: &[PipeWireGraphNode],
            clients: &[PipeWireGraphClient],
            ports: &[PipeWireGraphPort],
            links: &[PipeWireGraphLink],
            own_pids: &[i64],
            capture_node: u32,
        ) -> PipeWireGraphPlan;
        fn pipewire_graph_parse_int(text: &str, value: &mut i64) -> bool;
        fn pipewire_graph_stat_parent(stat: &str) -> i64;
        fn pipewire_graph_process_tree(self_pid: i64, pids: &[i64], parents: &[i64]) -> Vec<i64>;
    }
}

#[cfg(test)]
mod tests {
    use super::ffi::*;

    const ROOT: i64 = 100;
    const CHILD: i64 = 101;
    const CAPTURE: u32 = 90;
    const NONE: u32 = u32::MAX;
    const OUTPUT: &str = "Stream/Output/Audio";
    const INPUT: &str = "Stream/Input/Audio";
    const SINK: &str = "Audio/Sink";

    /// Port `i` of node `n` is 1000 + 10 n + i, so ports never collide with
    /// nodes (PipeWire ids share one space).
    fn port(node: u32, i: u32) -> u32 {
        1000 + 10 * node + i
    }

    fn link(output: u32, input: u32) -> PipeWireGraphLink {
        PipeWireGraphLink { output_port: output, input_port: input }
    }

    #[derive(Default)]
    struct Graph {
        nodes: Vec<PipeWireGraphNode>,
        clients: Vec<PipeWireGraphClient>,
        ports: Vec<PipeWireGraphPort>,
        links: Vec<PipeWireGraphLink>,
    }

    impl Graph {
        /// Root's capture node with its FL and FR inputs (ports 1901, 1902).
        fn new() -> Self {
            let mut graph = Self::default();
            graph.node(CAPTURE, INPUT, ROOT, NONE, "");
            graph.ports(CAPTURE, false, &["FL", "FR"]);
            graph
        }

        fn node(&mut self, id: u32, class: &str, pid: i64, client: u32, group: &str) -> &mut Self {
            self.nodes.push(PipeWireGraphNode {
                id,
                media_class: class.into(),
                pid,
                client_id: client,
                link_group: group.into(),
            });
            self
        }

        /// An application output stream with FL/FR ports.
        fn app(&mut self, id: u32, pid: i64, client: u32) -> &mut Self {
            self.node(id, OUTPUT, pid, client, "");
            self.ports(id, true, &["FL", "FR"]);
            self
        }

        fn client(&mut self, id: u32, pid: i64) -> &mut Self {
            self.clients.push(PipeWireGraphClient { id, pid });
            self
        }

        /// Ports numbered after the node's existing ones, named like PipeWire's
        /// ("output_FL", "input_FR").
        fn ports(&mut self, node: u32, output: bool, channels: &[&str]) -> Vec<u32> {
            let first = self.ports.iter().filter(|p| p.node_id == node).count() as u32 + 1;
            let prefix = if output { "output" } else { "input" };
            channels
                .iter()
                .enumerate()
                .map(|(i, channel)| {
                    let id = port(node, first + i as u32);
                    self.ports.push(PipeWireGraphPort {
                        id,
                        node_id: node,
                        output,
                        name: format!("{prefix}_{channel}"),
                        channel: channel.to_string(),
                    });
                    id
                })
                .collect()
        }

        /// Links node `from`'s outputs to node `to`'s inputs in order.
        fn connect(&mut self, from: u32, to: u32) -> &mut Self {
            let outputs: Vec<u32> =
                self.ports.iter().filter(|p| p.node_id == from && p.output).map(|p| p.id).collect();
            let inputs: Vec<u32> =
                self.ports.iter().filter(|p| p.node_id == to && !p.output).map(|p| p.id).collect();
            for (output, input) in outputs.iter().zip(inputs.iter()) {
                self.links.push(link(*output, *input));
            }
            self
        }

        fn plan(&self, own: &[i64]) -> PipeWireGraphPlan {
            pipewire_graph_plan(&self.nodes, &self.clients, &self.ports, &self.links, own, CAPTURE)
        }
    }

    fn sorted(mut links: Vec<PipeWireGraphLink>) -> Vec<PipeWireGraphLink> {
        links.sort();
        links
    }

    fn stereo(node: u32) -> Vec<PipeWireGraphLink> {
        vec![link(port(node, 1), port(CAPTURE, 1)), link(port(node, 2), port(CAPTURE, 2))]
    }

    /// A sink with FL/FR inputs and FL/FR monitor outputs.
    fn sink(graph: &mut Graph, id: u32, client: u32, group: &str) {
        graph.node(id, SINK, -1, client, group);
        graph.ports(id, false, &["FL", "FR"]);
        graph.ports(id, true, &["FL", "FR"]);
    }

    #[test]
    fn links_every_application_stream_channel_by_channel() {
        let mut graph = Graph::new();
        graph.app(30, ROOT, NONE).app(40, 500, NONE);
        let plan = graph.plan(&[ROOT]);
        assert!(plan.have_inputs);
        assert_eq!(plan.linked, vec![40]);
        assert_eq!(plan.own, vec![30]);
        assert!(plan.reemitters.is_empty());
        assert_eq!(sorted(plan.links), stereo(40));
    }

    #[test]
    fn a_stream_without_a_pid_takes_its_clients() {
        let mut graph = Graph::new();
        graph.client(7, CHILD).client(8, 600);
        graph.app(31, -1, 7).app(41, -1, 8).app(42, -1, NONE);
        let plan = graph.plan(&[ROOT, CHILD]);
        assert_eq!(plan.own, vec![31]);
        assert_eq!(plan.linked, vec![41, 42]);
    }

    #[test]
    fn the_node_pid_wins_over_the_clients() {
        let mut graph = Graph::new();
        graph.client(7, ROOT).app(41, 600, 7);
        let plan = graph.plan(&[ROOT]);
        assert_eq!(plan.linked, vec![41]);
        assert!(plan.own.is_empty());
    }

    #[test]
    fn mono_feeds_both_inputs() {
        let mut graph = Graph::new();
        graph.node(40, OUTPUT, 500, NONE, "");
        graph.ports(40, true, &["MONO"]);
        let plan = graph.plan(&[ROOT]);
        assert_eq!(
            sorted(plan.links),
            vec![link(port(40, 1), port(CAPTURE, 1)), link(port(40, 1), port(CAPTURE, 2))]
        );
    }

    #[test]
    fn other_layouts_alternate_and_port_names_stand_in_for_channels() {
        let mut graph = Graph::new();
        graph.node(40, OUTPUT, 500, NONE, "");
        graph.ports(40, true, &["FC", "LFE", "RL", "RR"]);
        graph.node(41, OUTPUT, 501, NONE, "");
        for (i, name) in ["playback_fl", "playback_fr"].iter().enumerate() {
            graph.ports.push(PipeWireGraphPort {
                id: port(41, i as u32 + 1),
                node_id: 41,
                output: true,
                name: name.to_string(),
                channel: String::new(),
            });
        }
        graph.node(42, OUTPUT, 502, NONE, "");
        graph.ports(42, true, &["fr"]);
        let plan = graph.plan(&[ROOT]);
        let (left, right) = (port(CAPTURE, 1), port(CAPTURE, 2));
        let mut expected = vec![
            link(port(40, 1), left),
            link(port(40, 2), right),
            link(port(40, 3), left),
            link(port(40, 4), right),
            link(port(41, 1), left),
            link(port(41, 2), right),
            link(port(42, 1), right),
        ];
        expected.sort();
        assert_eq!(sorted(plan.links), expected);
    }

    #[test]
    fn nothing_is_planned_before_the_capture_node_has_ports() {
        let mut graph = Graph::default();
        graph.node(CAPTURE, INPUT, ROOT, NONE, "");
        graph.app(40, 500, NONE);
        let plan = graph.plan(&[ROOT]);
        assert!(!plan.have_inputs);
        assert!(plan.links.is_empty());
        assert!(plan.linked.is_empty());
    }

    #[test]
    fn a_stream_without_output_ports_waits() {
        let mut graph = Graph::new();
        graph.node(40, OUTPUT, 500, NONE, "");
        let plan = graph.plan(&[ROOT]);
        assert_eq!(plan.no_ports, vec![40]);
        assert!(plan.links.is_empty());
    }

    #[test]
    fn one_capture_port_takes_both_channels() {
        let mut graph = Graph::default();
        graph.node(CAPTURE, INPUT, ROOT, NONE, "");
        graph.ports(CAPTURE, false, &["AUX0"]);
        graph.app(40, 500, NONE);
        let plan = graph.plan(&[ROOT]);
        let input = port(CAPTURE, 1);
        assert_eq!(sorted(plan.links), vec![link(port(40, 1), input), link(port(40, 2), input)]);
    }

    #[test]
    fn an_effects_app_downstream_of_root_is_excluded_and_its_sources_stay() {
        // Root and a game play into the effects sink; EasyEffects captures its
        // monitor and plays the processed mix to the hardware.
        let mut graph = Graph::new();
        sink(&mut graph, 10, 1, "");
        sink(&mut graph, 20, 2, "");
        graph.app(30, ROOT, 3).app(60, 800, 6);
        graph.node(50, INPUT, 700, 5, "");
        graph.ports(50, false, &["FL", "FR"]);
        graph.app(51, 700, 5);
        graph.connect(30, 20).connect(60, 20).connect(20, 50).connect(51, 10);
        let plan = graph.plan(&[ROOT]);
        assert_eq!(plan.reemitters, vec![51]);
        assert_eq!(plan.own, vec![30]);
        assert_eq!(plan.linked, vec![60]);
        assert_eq!(sorted(plan.links), stereo(60));
    }

    #[test]
    fn loopback_halves_are_tied_by_their_link_group() {
        // Both loopbacks belong to the daemon (one client, one pid); only the
        // one Root plays into re-emits Root.
        let mut graph = Graph::new();
        sink(&mut graph, 21, 2, "loopback-1");
        graph.node(22, OUTPUT, 50, 2, "loopback-1");
        graph.ports(22, true, &["FL", "FR"]);
        sink(&mut graph, 23, 2, "loopback-2");
        graph.node(24, OUTPUT, 50, 2, "loopback-2");
        graph.ports(24, true, &["FL", "FR"]);
        graph.app(30, ROOT, 3);
        graph.connect(30, 21);
        let plan = graph.plan(&[ROOT]);
        assert_eq!(plan.reemitters, vec![22]);
        assert_eq!(plan.linked, vec![24]);
    }

    #[test]
    fn chained_effects_are_followed() {
        let mut graph = Graph::new();
        sink(&mut graph, 10, 1, "");
        sink(&mut graph, 20, 1, "");
        sink(&mut graph, 24, 1, "");
        graph.app(30, ROOT, 3);
        graph.node(50, INPUT, 700, 5, "");
        graph.ports(50, false, &["FL", "FR"]);
        graph.app(51, 700, 5);
        graph.node(52, INPUT, 710, 7, "");
        graph.ports(52, false, &["FL", "FR"]);
        graph.app(53, 710, 7);
        graph.connect(30, 20).connect(20, 50).connect(51, 24).connect(24, 52).connect(53, 10);
        let plan = graph.plan(&[ROOT]);
        assert_eq!(plan.reemitters, vec![51, 53]);
        assert!(plan.linked.is_empty());
    }

    #[test]
    fn a_recorder_fed_directly_by_root_is_tainted() {
        let mut graph = Graph::new();
        graph.app(30, ROOT, 3);
        graph.node(55, INPUT, 900, 9, "");
        graph.ports(55, false, &["FL", "FR"]);
        graph.app(56, 900, 9).app(57, 901, 10);
        graph.connect(30, 55);
        let plan = graph.plan(&[ROOT]);
        assert_eq!(plan.reemitters, vec![56]);
        assert_eq!(plan.linked, vec![57]);
    }

    #[test]
    fn the_capture_node_never_taints_its_own_producer() {
        // The capture node (client 3) captures the sink Root plays into; were it
        // counted as a capturer, client 3's other output stream would be excluded.
        let mut graph = Graph::default();
        graph.node(CAPTURE, INPUT, -1, 3, "");
        graph.ports(CAPTURE, false, &["FL", "FR"]);
        sink(&mut graph, 10, 1, "");
        graph.app(30, ROOT, 4).app(34, 999, 3).app(60, 800, 6);
        graph.connect(30, 10).connect(60, 10).connect(10, CAPTURE);
        let plan = graph.plan(&[ROOT]);
        assert!(plan.reemitters.is_empty());
        assert_eq!(plan.linked, vec![34, 60]);
    }

    #[test]
    fn a_stream_that_became_a_reemitter_loses_its_link() {
        // The capture node already has a link from 51 (made before the default
        // sink moved onto the effects chain); the plan no longer wants it, so
        // the backend removes it.
        let mut graph = Graph::new();
        sink(&mut graph, 10, 1, "");
        sink(&mut graph, 20, 2, "");
        graph.app(30, ROOT, 3);
        graph.node(50, INPUT, 700, 5, "");
        graph.ports(50, false, &["FL", "FR"]);
        graph.app(51, 700, 5);
        graph.connect(30, 20).connect(20, 50).connect(51, 10).connect(51, CAPTURE);
        let plan = graph.plan(&[ROOT]);
        assert_eq!(plan.reemitters, vec![51]);
        assert!(plan.links.is_empty());
    }

    #[test]
    fn without_links_nothing_is_a_reemitter() {
        let mut graph = Graph::new();
        graph.node(22, OUTPUT, 50, 2, "loopback-1");
        graph.ports(22, true, &["FL", "FR"]);
        graph.app(30, ROOT, 3);
        let plan = graph.plan(&[ROOT]);
        assert!(plan.reemitters.is_empty());
        assert_eq!(plan.own, vec![30]);
        assert_eq!(plan.linked, vec![22]);
    }

    #[test]
    fn property_integers_parse_like_the_managed_parser() {
        let parse = |text: &str| {
            let mut value = 0;
            pipewire_graph_parse_int(text, &mut value).then_some(value)
        };
        assert_eq!(parse("42"), Some(42));
        assert_eq!(parse(" 42 \n"), Some(42));
        assert_eq!(parse("-7"), Some(-7));
        assert_eq!(parse("+5"), Some(5));
        assert_eq!(parse(""), None);
        assert_eq!(parse(" "), None);
        assert_eq!(parse("4a"), None);
        assert_eq!(parse("0x10"), None);
        assert_eq!(parse("99999999999999999999"), None);
    }

    #[test]
    fn stat_parent_is_the_field_after_the_last_parenthesis() {
        assert_eq!(pipewire_graph_stat_parent("1234 (bash) S 1000 1234 1234 0 -1"), 1000);
        assert_eq!(pipewire_graph_stat_parent("42 (my (odd) app) R 7 42 42"), 7);
        assert_eq!(pipewire_graph_stat_parent("42 (x) S 9\n"), 9);
        assert_eq!(pipewire_graph_stat_parent("1 (systemd) S 0 1 1"), 0);
        assert_eq!(pipewire_graph_stat_parent("garbage"), -1);
        assert_eq!(pipewire_graph_stat_parent("5 (x)"), -1);
        assert_eq!(pipewire_graph_stat_parent("5 (x) S"), -1);
    }

    #[test]
    fn the_process_tree_holds_every_descendant() {
        // Grandchildren listed before their parents still join.
        let pairs = [(103, 102), (102, 101), (101, 100), (200, 1), (201, 200), (104, 100)];
        let pids: Vec<i64> = pairs.iter().map(|p| p.0).collect();
        let parents: Vec<i64> = pairs.iter().map(|p| p.1).collect();
        let mut tree = pipewire_graph_process_tree(100, &pids, &parents);
        tree.sort();
        assert_eq!(tree, vec![100, 101, 102, 103, 104]);
        assert_eq!(pipewire_graph_process_tree(100, &[], &[]), vec![100]);
    }
}
