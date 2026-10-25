// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::{HashMap, HashSet};

use crate::routing::{
    MEDIA_CLASS_PLAYBACK_STREAM, PropMap, RoutingRule, SelfIdentity, matches_any, should_route_node,
};

pub(crate) struct CaptureGraph<'a> {
    nodes: &'a HashMap<u32, PropMap>,
    pending: &'a HashSet<u32>,
}

fn target(props: &PropMap) -> Option<&str> {
    props
        .get("target.object")
        .or_else(|| props.get("node.target"))
        .map(String::as_str)
}

fn group(props: &PropMap) -> Option<&str> {
    props
        .get("node.link-group")
        .map(String::as_str)
        .filter(|value| !value.is_empty())
}

impl<'a> CaptureGraph<'a> {
    pub(crate) fn new(nodes: &'a HashMap<u32, PropMap>, pending: &'a HashSet<u32>) -> Self {
        Self { nodes, pending }
    }

    fn is_sink_mix(&self, props: &PropMap) -> bool {
        self.nodes.iter().any(|(id, sink)| {
            if sink.get("media.class").map(String::as_str) != Some("Audio/Sink") {
                return false;
            }
            if let Some(output_group) = group(props) {
                if group(sink) == Some(output_group) {
                    return true;
                }
            }
            // Do not admit a mix while its sibling sink's full info is still pending.
            self.pending.contains(id)
                && props.get("client.id").is_some()
                && props.get("client.id") == sink.get("client.id")
        })
    }

    fn destinations(&self, props: &PropMap) -> Vec<PropMap> {
        let mut views = vec![props.clone()];
        let mut targets = target(props)
            .map(str::to_owned)
            .into_iter()
            .collect::<Vec<_>>();
        let mut seen = HashSet::new();
        let mut visited_sinks = HashSet::new();
        while let Some(destination) = targets.pop() {
            if !seen.insert(destination.clone()) {
                continue;
            }
            let mut view = props.clone();
            view.insert("target.object".into(), destination.clone());
            views.push(view);
            for (id, sink) in self.nodes {
                if self.pending.contains(id)
                    || sink.get("media.class").map(String::as_str) != Some("Audio/Sink")
                    || !(sink.get("node.name") == Some(&destination)
                        || sink.get("object.serial") == Some(&destination)
                        || destination.parse::<u32>().ok() == Some(*id))
                    || !visited_sinks.insert(*id)
                {
                    continue;
                }
                targets.extend(
                    ["node.name", "object.serial"]
                        .into_iter()
                        .filter_map(|key| sink.get(key).cloned()),
                );
                let Some(sink_group) = group(sink) else {
                    continue;
                };
                for (output_id, output) in self.nodes {
                    if self.pending.contains(output_id)
                        || output.get("media.class").map(String::as_str)
                            != Some(MEDIA_CLASS_PLAYBACK_STREAM)
                        || group(output) != Some(sink_group)
                    {
                        continue;
                    }
                    if let Some(next) = target(output) {
                        targets.push(next.to_owned());
                    } else {
                        let mut default_view = props.clone();
                        default_view.remove("target.object");
                        default_view.remove("node.target");
                        views.push(default_view);
                    }
                }
            }
        }
        views
    }

    pub(crate) fn should_route(
        &self,
        id: u32,
        rule: &RoutingRule,
        default_sink_name: &str,
        default_sink_target_id: &str,
        sink_global_id: u32,
        identity: &SelfIdentity,
    ) -> bool {
        let Some(props) = self.nodes.get(&id) else {
            return false;
        };
        if self.pending.contains(&id)
            || props.get("media.class").map(String::as_str) != Some(MEDIA_CLASS_PLAYBACK_STREAM)
            || self.is_sink_mix(props)
        {
            return false;
        }
        // Capture original application streams, before a virtual sink mixes in call audio.
        // Follow the sink's output destination so selecting physical speakers still includes
        // applications playing through a virtual surround/EQ sink.
        let destinations = self.destinations(props);
        if destinations
            .iter()
            .any(|view| matches_any(view, &rule.never_when))
        {
            return false;
        }
        destinations.iter().any(|view| {
            should_route_node(
                id,
                view,
                rule,
                default_sink_name,
                default_sink_target_id,
                sink_global_id,
                identity,
            )
        })
    }
}

#[cfg(test)]
mod tests;
