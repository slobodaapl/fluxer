// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;

fn props(entries: &[(&str, &str)]) -> PropMap {
    entries
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

fn surround_graph() -> HashMap<u32, PropMap> {
    HashMap::from([
        (
            57,
            props(&[
                ("media.class", "Audio/Sink"),
                ("node.name", "virtual.arctis"),
                ("object.serial", "10057"),
                ("node.link-group", "surround"),
                ("client.id", "49"),
            ]),
        ),
        (
            58,
            props(&[
                ("media.class", MEDIA_CLASS_PLAYBACK_STREAM),
                ("node.name", "effect_output.arctis"),
                ("node.link-group", "surround"),
                ("client.id", "49"),
                ("node.virtual", "true"),
                ("target.object", "headset"),
            ]),
        ),
        (
            167,
            props(&[
                ("media.class", "Audio/Sink"),
                ("node.name", "headset"),
                ("object.serial", "10167"),
            ]),
        ),
        (
            312,
            props(&[
                ("media.class", MEDIA_CLASS_PLAYBACK_STREAM),
                ("application.name", "Fluxer"),
                ("target.object", "virtual.arctis"),
            ]),
        ),
        (
            400,
            props(&[
                ("media.class", MEDIA_CLASS_PLAYBACK_STREAM),
                ("application.name", "Game"),
                ("target.object", "virtual.arctis"),
            ]),
        ),
    ])
}

fn system_rule() -> RoutingRule {
    RoutingRule {
        only_audio_sinks: true,
        only_default_audio_sink: true,
        ..Default::default()
    }
}

fn selected(
    nodes: &HashMap<u32, PropMap>,
    pending: &HashSet<u32>,
    rule: &RoutingRule,
    sink: &str,
) -> Vec<u32> {
    let graph = CaptureGraph::new(nodes, pending);
    let mut identity = SelfIdentity::default();
    identity.add_display_name("Fluxer");
    let mut sources = nodes
        .keys()
        .copied()
        .filter(|id| graph.should_route(*id, rule, sink, "", 999, &identity))
        .collect::<Vec<_>>();
    sources.sort_unstable();
    sources
}

#[test]
fn surround_mix_cannot_reintroduce_call_audio_or_duplicate_the_game() {
    let nodes = surround_graph();
    for sink in ["headset", "virtual.arctis", ""] {
        assert_eq!(
            selected(&nodes, &HashSet::new(), &system_rule(), sink),
            [400]
        );
    }
}

#[test]
fn device_selection_and_exclusion_follow_virtual_sink_destinations() {
    let nodes = surround_graph();
    let mut rule = RoutingRule {
        include_when: vec![props(&[("fluxer.target.objects", "10167")])],
        ..Default::default()
    };
    assert_eq!(selected(&nodes, &HashSet::new(), &rule, ""), [400]);
    rule.never_when
        .push(props(&[("fluxer.target.objects", "virtual.arctis")]));
    assert!(selected(&nodes, &HashSet::new(), &rule, "").is_empty());
    rule.never_when = vec![props(&[("fluxer.target.objects", "headset")])];
    assert!(selected(&nodes, &HashSet::new(), &rule, "").is_empty());
}

#[test]
fn explicitly_including_fluxer_or_the_mixed_output_does_not_capture_either() {
    let nodes = surround_graph();
    let rule = RoutingRule {
        include_when: vec![
            props(&[("application.name", "Fluxer")]),
            props(&[("node.name", "effect_output.arctis")]),
        ],
        ..Default::default()
    };
    assert!(selected(&nodes, &HashSet::new(), &rule, "").is_empty());
}

#[test]
fn pending_node_info_never_temporarily_admits_the_surround_mix() {
    let mut nodes = surround_graph();
    nodes.get_mut(&57).unwrap().remove("node.link-group");
    let pending = HashSet::from([57, 400]);
    assert!(selected(&nodes, &pending, &system_rule(), "").is_empty());
    nodes
        .get_mut(&57)
        .unwrap()
        .insert("node.link-group".into(), "surround".into());
    assert_eq!(
        selected(&nodes, &HashSet::new(), &system_rule(), "headset"),
        [400]
    );
}

#[test]
fn application_virtual_streams_are_not_blanket_excluded() {
    let mut nodes = surround_graph();
    nodes.insert(
        401,
        props(&[
            ("media.class", MEDIA_CLASS_PLAYBACK_STREAM),
            ("application.name", "Synth"),
            ("node.virtual", "true"),
            ("node.link-group", "synth"),
            ("target.object", "headset"),
        ]),
    );
    assert_eq!(
        selected(&nodes, &HashSet::new(), &system_rule(), "headset"),
        [400, 401]
    );
}

#[test]
fn destination_changes_and_removal_do_not_keep_old_routes() {
    let mut nodes = surround_graph();
    nodes
        .get_mut(&58)
        .unwrap()
        .insert("target.object".into(), "other-speakers".into());
    assert!(selected(&nodes, &HashSet::new(), &system_rule(), "headset").is_empty());
    assert_eq!(
        selected(&nodes, &HashSet::new(), &system_rule(), "other-speakers"),
        [400]
    );
    nodes.remove(&400);
    assert!(selected(&nodes, &HashSet::new(), &system_rule(), "other-speakers").is_empty());
}

#[test]
fn chained_filters_use_names_serials_and_ids_without_following_cycles_forever() {
    let mut nodes = surround_graph();
    nodes
        .get_mut(&58)
        .unwrap()
        .insert("target.object".into(), "600".into());
    nodes.insert(
        600,
        props(&[
            ("media.class", "Audio/Sink"),
            ("node.name", "virtual.eq"),
            ("object.serial", "10600"),
            ("node.link-group", "eq"),
        ]),
    );
    nodes.insert(
        601,
        props(&[
            ("media.class", MEDIA_CLASS_PLAYBACK_STREAM),
            ("node.link-group", "eq"),
            ("target.object", "10167"),
        ]),
    );
    assert_eq!(
        selected(&nodes, &HashSet::new(), &system_rule(), "headset"),
        [400]
    );
    nodes
        .get_mut(&601)
        .unwrap()
        .insert("target.object".into(), "virtual.arctis".into());
    assert!(selected(&nodes, &HashSet::new(), &system_rule(), "headset").is_empty());
}
