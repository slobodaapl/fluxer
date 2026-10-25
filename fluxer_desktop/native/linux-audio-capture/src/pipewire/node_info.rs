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
            let Some(props) = info.props() else { return };
            let Ok(mut snapshot) = inventory.lock() else {
                return;
            };
            let Some(current) = snapshot.nodes.get_mut(&id) else {
                return;
            };
            let next = collect_props(Some(props));
            let changed = *current != next;
            *current = next;
            let first_info = snapshot.pending_node_info.remove(&id);
            drop(snapshot);
            if first_info || changed {
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
