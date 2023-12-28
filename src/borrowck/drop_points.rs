use std::collections::HashMap;

use petgraph::{stable_graph::NodeIndex, Direction};

use crate::id::{NodeID, VariableID};

use super::control_flow_graph::{CfgNode, ControlFlowGraph, FunctionCFG, Liveness};

/// Node ID -> Vars to drop
pub fn find_drop_points(cfg: &FunctionCFG) -> HashMap<NodeID, Vec<VariableID>> {
    let mut map = HashMap::new();
    let CfgNode::Exit { liveness } = cfg.cfg.node_weight(cfg.end).unwrap() else {
        unreachable!()
    };
    for var_id in liveness.keys() {
        find_drop_points_node(&cfg.cfg, &mut map, var_id, cfg.end);
    }
    map
}

fn find_drop_points_node(
    cfg: &ControlFlowGraph,
    map: &mut HashMap<NodeID, Vec<VariableID>>,
    variable: &VariableID,
    node: NodeIndex,
) {
    for parent in cfg.neighbors_directed(node, Direction::Incoming) {
        let CfgNode::Block { liveness, .. } = cfg.node_weight(parent).unwrap() else {
            unreachable!()
        };
        match liveness[variable] {
            // If the variable is moved in this branch, don't drop
            Liveness::Moved => {}
            // If the variable is moved or referenced in some branches, drop in those only
            Liveness::ParentConditionalMoved(_) | Liveness::ParentReferenced => {
                find_drop_points_node(cfg, map, variable, parent);
            }
            // The variable is referenced here. Drop after this
            Liveness::Referenced(id) => {
                map.entry(id).or_default().push(*variable);
            }
        }
    }
}