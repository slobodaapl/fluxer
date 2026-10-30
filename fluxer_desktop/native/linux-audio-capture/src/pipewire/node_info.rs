// SPDX-License-Identifier: AGPL-3.0-or-later

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use pipewire as pw;

use super::common::{InventorySnapshot, collect_props};

pub(super) struct NodeWatch {
    _listener: pw::node::NodeListener,
    _node: pw::node::Node,
}

pub(super) type NodeWatches = Rc<RefCell<HashMap<u32, NodeWatch>>>;

fn apply_node_info(
    snapshot: &mut InventorySnapshot,
    id: u32,
    mask: pw::node::NodeChangeMask,
    props: Option<&pw::spa::utils::dict::DictRef>,
) -> bool {
    if !mask.contains(pw::node::NodeChangeMask::PROPS) {
        return false;
    }
    let Some(props) = props else { return false };
    let Some(current) = snapshot.nodes.get_mut(&id) else {
        return false;
    };
    let next = collect_props(Some(props));
    let changed = *current != next;
    *current = next;
    snapshot.pending_node_info.remove(&id) || changed
}

pub(super) fn watch_node(
    obj: &pw::registry::GlobalObject<&pw::spa::utils::dict::DictRef>,
    registry: &pw::registry::RegistryRc,
    inventory: &Arc<Mutex<InventorySnapshot>>,
    watches: &NodeWatches,
    on_change: impl Fn() + 'static,
) {
    if obj.type_ != pw::types::ObjectType::Node {
        return;
    }
    let Ok(snapshot) = inventory.lock() else {
        return;
    };
    if !snapshot.nodes.contains_key(&obj.id) {
        return;
    }
    drop(snapshot);
    let Ok(node) = registry.bind::<pw::node::Node, _>(obj) else {
        return;
    };
    let inventory = inventory.clone();
    let id = obj.id;
    let listener = node
        .add_listener_local()
        .info(move |info| {
            let Ok(mut snapshot) = inventory.lock() else {
                return;
            };
            let changed = apply_node_info(&mut snapshot, id, info.change_mask(), info.props());
            drop(snapshot);
            if changed {
                on_change();
            }
        })
        .register();
    watches.borrow_mut().insert(
        id,
        NodeWatch {
            _listener: listener,
            _node: node,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use pw::node::NodeChangeMask;

    #[test]
    fn state_updates_preserve_identity_until_properties_change() {
        let mut snapshot = InventorySnapshot::default();
        let initial = pw::properties::properties! {
            "media.class" => "Stream/Output/Audio",
            "application.name" => "Game",
            "target.object" => "old-sink",
        };
        snapshot
            .nodes
            .insert(42, collect_props(Some(initial.dict())));
        snapshot.pending_node_info.insert(42);
        let empty = pw::properties::PropertiesBox::new();
        assert!(!apply_node_info(
            &mut snapshot,
            42,
            NodeChangeMask::STATE,
            Some(empty.dict())
        ));
        assert!(snapshot.pending_node_info.contains(&42));
        assert_eq!(snapshot.nodes[&42]["application.name"], "Game");
        assert!(apply_node_info(
            &mut snapshot,
            42,
            NodeChangeMask::PROPS,
            Some(initial.dict())
        ));
        assert!(!snapshot.pending_node_info.contains(&42));
        assert!(!apply_node_info(
            &mut snapshot,
            42,
            NodeChangeMask::OUTPUT_PORTS | NodeChangeMask::STATE,
            Some(empty.dict())
        ));
        assert_eq!(snapshot.nodes[&42]["target.object"], "old-sink");
        let updated = pw::properties::properties! {
            "media.class" => "Stream/Output/Audio",
            "application.name" => "Game",
        };
        assert!(apply_node_info(
            &mut snapshot,
            42,
            NodeChangeMask::PROPS,
            Some(updated.dict())
        ));
        assert!(!snapshot.nodes[&42].contains_key("target.object"));
        assert!(!apply_node_info(
            &mut snapshot,
            999,
            NodeChangeMask::PROPS,
            Some(initial.dict())
        ));
    }
}
