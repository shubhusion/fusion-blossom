//! Serial Dual Module
//! 
//! A serial implementation of the dual module. This is the very basic fusion blossom algorithm that aims at debugging and as a ground truth
//! where traditional matching is too time consuming because of their |E| = O(|V|^2) scaling.
//! 
//! This implementation supports fast clear: optimized for a small number of syndrome and small cluster coverage, the ``clear growth'' operator
//! can be executed in O(1) time, at the cost of dynamic check and dynamic reset. This also increases cache coherency, because a global clear
//! operation is unfriendly to cache.
//!

use super::util::*;
use crate::derivative::Derivative;
use std::sync::{Arc, Weak};
use crate::parking_lot::RwLock;
use super::dual_module::*;
use super::visualize::*;
use std::collections::HashMap;
use crate::weak_table::PtrWeakKeyHashMap;


pub struct DualModuleSerial {
    /// all vertices including virtual ones
    pub vertices: Vec<VertexPtr>,
    /// nodes internal information
    pub nodes: Vec<Option<DualNodeInternalPtr>>,
    /// keep edges, which can also be accessed in [`Self::vertices`]
    pub edges: Vec<EdgePtr>,
    /// current timestamp
    pub active_timestamp: FastClearTimestamp,
    /// the number of all vertices (including those partitioned into other serial modules)
    pub vertex_num: usize,
    /// the number of all edges (including those partitioned into other serial modules)
    pub edge_num: usize,
    /// vertices exclusively owned by this module, useful when partitioning the decoding graph into multiple [`DualModuleSerial`]
    pub owning_range: VertexRange,
    /// module information when used as a component in the partitioned dual module
    pub unit_module_info: Option<UnitModuleInfo>,
    /// maintain an active list to optimize for average cases: most syndrome vertices have already been matched, and we only need to work on a few remained;
    /// note that this list may contain deleted node as well as duplicate nodes
    pub active_list: Vec<DualNodeInternalWeak>,
    /// helps to deduplicate [`DualModuleSerial::active_list`]
    current_cycle: usize,
    /// remember the edges that's modified by erasures
    pub edge_modifier: EdgeWeightModifier,
}

/// synchronize request on vertices, when a vertex is mirrored
#[derive(Derivative)]
#[derivative(Debug)]
pub struct SyncRequest {
    /// the unit that owns this vertex
    pub mirror_unit_weak: PartitionUnitWeak,
    /// the vertex index to be synchronized
    pub vertex_index: VertexIndex,
    /// propagated dual node index
    pub propagated_dual_node: Option<DualNodeWeak>,
    /// propagated grandson node: must be a syndrome node
    pub propagated_grandson_dual_node: Option<DualNodeWeak>,
}

/// records information only available when used as a unit in the partitioned dual module
#[derive(Derivative)]
#[derivative(Debug)]
pub struct UnitModuleInfo {
    /// unit index
    pub unit_index: usize,
    /// all mirrored vertices (excluding owned ones) to query if this module contains the vertex
    pub mirrored_vertices: HashMap<VertexIndex, usize>,
    /// owned dual nodes range
    pub owning_dual_range: VertexRange,
    /// hash table for mapping [`DualNodePtr`] to internal [`DualNodeInternalPtr`]
    pub dual_node_pointers: PtrWeakKeyHashMap<DualNodeWeak, usize>,
    /// temporary list of synchronize requests, i.e. those propagating into the mirrored vertices
    pub sync_requests: Vec<SyncRequest>,
}

create_ptr_types!(DualModuleSerial, DualModuleSerialPtr, DualModuleSerialWeak);

/// internal information of the dual node, added to the [`DualNode`]
#[derive(Derivative)]
#[derivative(Debug)]
pub struct DualNodeInternal {
    /// the pointer to the origin [`DualNode`]
    pub origin: DualNodeWeak,
    /// local index, to find myself in [`DualModuleSerial::nodes`]
    index: NodeIndex,
    /// dual variable of this node
    pub dual_variable: Weight,
    /// edges on the boundary of this node, (`is_left`, `edge`)
    pub boundary: Vec<(bool, EdgeWeak)>,
    /// over-grown vertices on the boundary of this node, this is to solve a bug where all surrounding edges are fully grown
    /// so all edges are deleted from the boundary... this will lose track of the real boundary when shrinking back
    pub overgrown_stack: Vec<(VertexWeak, Weight)>,
    /// helps to prevent duplicate visit in a single cycle
    last_visit_cycle: usize,
}

create_ptr_types!(DualNodeInternal, DualNodeInternalPtr, DualNodeInternalWeak);

impl std::fmt::Debug for DualNodeInternalPtr {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let dual_node_internal = self.read_recursive();
        write!(f, "{}", dual_node_internal.index)
    }
}

impl std::fmt::Debug for DualNodeInternalWeak {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        self.upgrade_force().fmt(f)
    }
}

#[derive(Derivative)]
#[derivative(Debug)]
pub struct Vertex {
    /// the index of this vertex in the decoding graph, not necessary the index in [`DualModuleSerial::vertices`] if it's partitioned
    pub vertex_index: VertexIndex,
    /// if a vertex is virtual, then it can be matched any times
    pub is_virtual: bool,
    /// if a vertex is syndrome, then [`Vertex::propagated_dual_node`] always corresponds to that root
    pub is_syndrome: bool,
    /// if it's a mirrored vertex (present on multiple units), then this is the parallel unit that exclusively owns it
    pub mirror_unit: Option<PartitionUnitWeak>,
    /// all neighbor edges, in surface code this should be constant number of edges
    #[derivative(Debug="ignore")]
    pub edges: Vec<EdgeWeak>,
    /// propagated dual node
    pub propagated_dual_node: Option<DualNodeInternalWeak>,
    /// propagated grandson node: must be a syndrome node
    pub propagated_grandson_dual_node: Option<DualNodeInternalWeak>,
    /// for fast clear
    pub timestamp: FastClearTimestamp,
}

create_fast_clear_ptr_types!(Vertex, VertexPtr, VertexWeak);

impl std::fmt::Debug for VertexPtr {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let vertex = self.read_recursive_force();
        write!(f, "{}", vertex.vertex_index)
    }
}

impl std::fmt::Debug for VertexWeak {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        self.upgrade_force().fmt(f)
    }
}


#[derive(Derivative)]
#[derivative(Debug)]
pub struct Edge {
    /// global edge index, not necessary the index in [`DualModuleSerial::edges`]
    pub edge_index: EdgeIndex,
    /// total weight of this edge
    pub weight: Weight,
    /// left vertex (always with smaller index for consistency)
    #[derivative(Debug="ignore")]
    pub left: VertexWeak,
    /// right vertex (always with larger index for consistency)
    #[derivative(Debug="ignore")]
    pub right: VertexWeak,
    /// growth from the left point
    pub left_growth: Weight,
    /// growth from the right point
    pub right_growth: Weight,
    /// left active tree node (if applicable)
    pub left_dual_node: Option<DualNodeInternalWeak>,
    /// left grandson node: must be a syndrome node
    pub left_grandson_dual_node: Option<DualNodeInternalWeak>,
    /// right active tree node (if applicable)
    pub right_dual_node: Option<DualNodeInternalWeak>,
    /// left grandson node: must be a syndrome node
    pub right_grandson_dual_node: Option<DualNodeInternalWeak>,
    /// for fast clear
    pub timestamp: FastClearTimestamp,
}

create_fast_clear_ptr_types!(Edge, EdgePtr, EdgeWeak);

impl std::fmt::Debug for EdgePtr {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let edge = self.read_recursive_force();
        write!(f, "{}", edge.edge_index)
    }
}

impl std::fmt::Debug for EdgeWeak {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        self.upgrade_force().fmt(f)
    }
}

impl DualModuleImpl for DualModuleSerial {

    /// initialize the dual module, which is supposed to be reused for multiple decoding tasks with the same structure
    fn new(initializer: &SolverInitializer) -> Self {
        let active_timestamp = 0;
        // create vertices
        let vertices: Vec<VertexPtr> = (0..initializer.vertex_num).map(|vertex_index| VertexPtr::new(Vertex {
            vertex_index: vertex_index,
            is_virtual: false,
            is_syndrome: false,
            mirror_unit: None,
            edges: Vec::new(),
            propagated_dual_node: None,
            propagated_grandson_dual_node: None,
            timestamp: active_timestamp,
        })).collect();
        // set virtual vertices
        for &virtual_vertex in initializer.virtual_vertices.iter() {
            let mut vertex = vertices[virtual_vertex].write(active_timestamp);
            vertex.is_virtual = true;
        }
        // set edges
        let mut edges = Vec::<EdgePtr>::new();
        for &(i, j, weight) in initializer.weighted_edges.iter() {
            assert_ne!(i, j, "invalid edge from and to the same vertex {}", i);
            assert!(i < initializer.vertex_num, "edge ({}, {}) connected to an invalid vertex {}", i, j, i);
            assert!(j < initializer.vertex_num, "edge ({}, {}) connected to an invalid vertex {}", i, j, j);
            let left = usize::min(i, j);
            let right = usize::max(i, j);
            let edge_ptr = EdgePtr::new(Edge {
                edge_index: edges.len(),
                weight: weight,
                left: vertices[left].downgrade(),
                right: vertices[right].downgrade(),
                left_growth: 0,
                right_growth: 0,
                left_dual_node: None,
                left_grandson_dual_node: None,
                right_dual_node: None,
                right_grandson_dual_node: None,
                timestamp: 0,
            });
            for (a, b) in [(i, j), (j, i)] {
                let mut vertex = vertices[a].write(active_timestamp);
                debug_assert!({  // O(N^2) sanity check, debug mode only (actually this bug is not critical, only the shorter edge will take effect)
                    let mut no_duplicate = true;
                    for edge_weak in vertex.edges.iter() {
                        let edge_ptr = edge_weak.upgrade_force();
                        let edge = edge_ptr.read_recursive(active_timestamp);
                        if edge.left == vertices[b].downgrade() || edge.right == vertices[b].downgrade() {
                            no_duplicate = false;
                            eprintln!("duplicated edge between {} and {} with weight w1 = {} and w2 = {}, consider merge them into a single edge", i, j, weight, edge.weight);
                            break
                        }
                    }
                    no_duplicate
                });
                vertex.edges.push(edge_ptr.downgrade());
            }
            edges.push(edge_ptr);
        }
        Self {
            vertices: vertices,
            nodes: vec![],
            edges: edges,
            active_timestamp: 0,
            vertex_num: initializer.vertex_num,
            edge_num: initializer.weighted_edges.len(),
            owning_range: VertexRange::new(0, initializer.vertex_num),
            unit_module_info: None,  // disabled
            active_list: vec![],
            current_cycle: 0,
            edge_modifier: EdgeWeightModifier::new(),
        }
    }

    /// clear all growth and existing dual nodes
    fn clear(&mut self) {
        // recover erasure edges first
        while self.edge_modifier.has_modified_edges() {
            let (edge_index, original_weight) = self.edge_modifier.pop_modified_edge();
            let edge_ptr = &self.edges[edge_index];
            let mut edge = edge_ptr.write(self.active_timestamp);
            edge.weight = original_weight;
        }
        self.clear_graph();
        self.nodes.clear();
        if let Some(unit_module_info) = self.unit_module_info.as_mut() {
            unit_module_info.owning_dual_range = VertexRange::new(0, 0);
            unit_module_info.dual_node_pointers = PtrWeakKeyHashMap::<DualNodeWeak, usize>::new();
        }
        self.active_list.clear();
    }

    /// add a new dual node from dual module root
    fn add_dual_node(&mut self, dual_node_ptr: &DualNodePtr) {
        self.register_dual_node_ptr(dual_node_ptr);
        let active_timestamp = self.active_timestamp;
        let node = dual_node_ptr.read_recursive();
        let node_internal_ptr = DualNodeInternalPtr::new(DualNodeInternal {
            origin: dual_node_ptr.downgrade(),
            index: self.nodes.len(),
            dual_variable: 0,
            boundary: Vec::new(),
            overgrown_stack: Vec::new(),
            last_visit_cycle: 0,
        });
        {
            let boundary = &mut node_internal_ptr.write().boundary;
            match &node.class {
                DualNodeClass::Blossom { nodes_circle, .. } => {
                    // copy all the boundary edges and modify edge belongings
                    for dual_node_weak in nodes_circle.iter() {
                        let dual_node_ptr = dual_node_weak.upgrade_force();
                        if self.unit_module_info.is_none() {  // otherwise it's already done
                            self.prepare_dual_node_growth(&dual_node_ptr, false);  // prepare all nodes in shrinking mode for consistency
                        }
                        if let Some(dual_node_internal_ptr) = self.get_dual_node_internal_ptr_optional(&dual_node_ptr) {
                            let dual_node_internal = dual_node_internal_ptr.read_recursive();
                            for (is_left, edge_weak) in dual_node_internal.boundary.iter() {
                                let edge_ptr = edge_weak.upgrade_force();
                                boundary.push((*is_left, edge_weak.clone()));
                                let mut edge = edge_ptr.write(active_timestamp);
                                assert!(if *is_left { edge.left_dual_node.is_some() } else { edge.right_dual_node.is_some() }, "dual node of edge should be some");
                                debug_assert!(if *is_left { edge.left_dual_node == Some(dual_node_internal_ptr.downgrade())
                                    } else { edge.right_dual_node == Some(dual_node_internal_ptr.downgrade()) }, "edge belonging");
                                if *is_left {
                                    edge.left_dual_node = Some(node_internal_ptr.downgrade());
                                } else {
                                    edge.right_dual_node = Some(node_internal_ptr.downgrade());
                                }
                            }
                        } else {
                            assert!(self.unit_module_info.is_some(), "only partitioned could ignore some of its children");
                        }
                    }
                },
                DualNodeClass::SyndromeVertex { syndrome_index } => {
                    assert!(self.owning_range.contains(syndrome_index), "syndrome not belonging to this dual module");
                    let vertex_ptr = &self.vertices[*syndrome_index - self.owning_range.start()];  // exclusively owned vertices are always placed in the front
                    vertex_ptr.dynamic_clear(active_timestamp);
                    let mut vertex = vertex_ptr.write(active_timestamp);
                    vertex.propagated_dual_node = Some(node_internal_ptr.downgrade());
                    vertex.propagated_grandson_dual_node = Some(node_internal_ptr.downgrade());
                    vertex.is_syndrome = true;
                    for edge_weak in vertex.edges.iter() {
                        let edge_ptr = edge_weak.upgrade_force();
                        edge_ptr.dynamic_clear(active_timestamp);
                        let mut edge = edge_ptr.write(active_timestamp);
                        let is_left = vertex_ptr.downgrade() == edge.left;
                        assert!(if is_left { edge.left_dual_node.is_none() } else { edge.right_dual_node.is_none() }, "dual node of edge should be none");
                        if is_left {
                            edge.left_dual_node = Some(node_internal_ptr.downgrade());
                            edge.left_grandson_dual_node = Some(node_internal_ptr.downgrade());
                        } else {
                            edge.right_dual_node = Some(node_internal_ptr.downgrade());
                            edge.right_grandson_dual_node = Some(node_internal_ptr.downgrade());
                        }
                        boundary.push((is_left, edge_weak.clone()));
                    }
                },
            }
        }
        self.active_list.push(node_internal_ptr.downgrade());
        self.nodes.push(Some(node_internal_ptr));
    }

    fn remove_blossom(&mut self, dual_node_ptr: DualNodePtr) {
        let active_timestamp = self.active_timestamp;
        self.prepare_dual_node_growth(&dual_node_ptr, false);  // prepare the blossom into shrinking
        let node = dual_node_ptr.read_recursive();
        let dual_node_internal_ptr = self.get_dual_node_internal_ptr(&dual_node_ptr);
        let dual_node_internal = dual_node_internal_ptr.read_recursive();
        assert_eq!(dual_node_internal.dual_variable, 0, "only blossom with dual variable = 0 can be safely removed");
        assert!(dual_node_internal.overgrown_stack.len() == 0, "removing a blossom with non-empty overgrown stack forbidden");
        let node_idx = dual_node_internal.index;
        assert!(self.nodes[node_idx].is_some(), "blossom may have already been removed, do not call twice");
        assert!(self.nodes[node_idx].as_ref().unwrap() == &dual_node_internal_ptr, "the blossom doesn't belong to this DualModuleInterface");
        self.nodes[node_idx] = None;  // simply remove this blossom node
        // recover edge belongings
        for (is_left, edge_weak) in dual_node_internal.boundary.iter() {
            let edge_ptr = edge_weak.upgrade_force();
            let mut edge = edge_ptr.write(active_timestamp);
            assert!(if *is_left { edge.left_dual_node.is_some() } else { edge.right_dual_node.is_some() }, "dual node of edge should be some");
            if *is_left {
                edge.left_dual_node = None;
            } else {
                edge.right_dual_node = None;
            }
        }
        if let DualNodeClass::Blossom{ nodes_circle, .. } = &node.class {
            for circle_dual_node_weak in nodes_circle.iter() {
                let circle_dual_node_ptr = circle_dual_node_weak.upgrade_force();
                let circle_dual_node_internal_ptr = self.get_dual_node_internal_ptr(&circle_dual_node_ptr);
                let circle_dual_node_internal = circle_dual_node_internal_ptr.read_recursive();
                for (is_left, edge_weak) in circle_dual_node_internal.boundary.iter() {
                    let edge_ptr = edge_weak.upgrade_force();
                    let mut edge = edge_ptr.write(active_timestamp);
                    assert!(if *is_left { edge.left_dual_node.is_none() } else { edge.right_dual_node.is_none() }, "dual node of edge should be none");
                    if *is_left {
                        edge.left_dual_node = Some(circle_dual_node_internal_ptr.downgrade());
                    } else {
                        edge.right_dual_node = Some(circle_dual_node_internal_ptr.downgrade());
                    }
                }
            }
        } else {
            unreachable!()
        }
    }

    fn set_grow_state(&mut self, dual_node_ptr: &DualNodePtr, grow_state: DualNodeGrowState) {
        let dual_node = dual_node_ptr.read_recursive();
        if dual_node.grow_state == DualNodeGrowState::Stay && grow_state != DualNodeGrowState::Stay {
            let dual_node_internal_ptr = self.get_dual_node_internal_ptr(&dual_node_ptr);
            self.active_list.push(dual_node_internal_ptr.downgrade())
        }
    }

    fn compute_maximum_update_length_dual_node(&mut self, dual_node_ptr: &DualNodePtr, is_grow: bool, simultaneous_update: bool) -> MaxUpdateLength {
        let active_timestamp = self.active_timestamp;
        if !simultaneous_update {
            // when `simultaneous_update` is set, it's assumed that all nodes are prepared to grow or shrink
            // this is because if we dynamically prepare them, it would be inefficient
            self.prepare_dual_node_growth(dual_node_ptr, is_grow);
        }
        let mut max_length_abs = Weight::MAX;
        let dual_node_internal_ptr = self.get_dual_node_internal_ptr(&dual_node_ptr);
        let dual_node_internal = dual_node_internal_ptr.read_recursive();
        if !is_grow {
            if dual_node_internal.dual_variable == 0 {
                let dual_node = dual_node_ptr.read_recursive();
                match dual_node.class {
                    DualNodeClass::Blossom { .. } => { return MaxUpdateLength::BlossomNeedExpand(dual_node_ptr.downgrade()) }
                    DualNodeClass::SyndromeVertex { .. } => { return MaxUpdateLength::VertexShrinkStop(dual_node_ptr.downgrade()) }
                }
            }
            if dual_node_internal.overgrown_stack.len() > 0 {
                let last_index = dual_node_internal.overgrown_stack.len() - 1;
                let (_, overgrown) = &dual_node_internal.overgrown_stack[last_index];
                max_length_abs = std::cmp::min(max_length_abs, *overgrown);
            }
            max_length_abs = std::cmp::min(max_length_abs, dual_node_internal.dual_variable);
        }
        for (is_left, edge_weak) in dual_node_internal.boundary.iter() {
            let edge_ptr = edge_weak.upgrade_force();
            let is_left = *is_left;
            let edge = edge_ptr.read_recursive(active_timestamp);
            if is_grow {
                // first check if both side belongs to the same tree node, if so, no constraint on this edge
                let peer_dual_node_internal_ptr: Option<DualNodeInternalPtr> = if is_left {
                    edge.right_dual_node.as_ref().map(|ptr| ptr.upgrade_force())
                } else {
                    edge.left_dual_node.as_ref().map(|ptr| ptr.upgrade_force())
                };
                match peer_dual_node_internal_ptr {
                    Some(peer_dual_node_internal_ptr) => {
                        if peer_dual_node_internal_ptr == dual_node_internal_ptr {
                            continue
                        } else {
                            let peer_dual_node_internal = peer_dual_node_internal_ptr.read_recursive();
                            let peer_dual_node_ptr = peer_dual_node_internal.origin.upgrade_force();
                            let peer_dual_node = peer_dual_node_ptr.read_recursive();
                            let remaining_length = edge.weight - edge.left_growth - edge.right_growth;
                            let local_max_length_abs = match peer_dual_node.grow_state {
                                DualNodeGrowState::Grow => {
                                    assert!(remaining_length % 2 == 0, "there is odd gap between two growing nodes, please make sure all weights are even numbers");
                                    remaining_length / 2
                                },
                                DualNodeGrowState::Shrink => {
                                    // special case: if peer is a syndrome vertex and it's dual variable is already 0, 
                                    // then we need to determine if some other growing nodes are conflicting with me
                                    if remaining_length == 0 && matches!(peer_dual_node.class, DualNodeClass::SyndromeVertex{ .. }) && peer_dual_node_internal.dual_variable == 0 {
                                        for (peer_is_left, peer_edge_weak) in peer_dual_node_internal.boundary.iter() {
                                            let peer_edge_ptr = peer_edge_weak.upgrade_force();
                                            let peer_edge = peer_edge_ptr.read_recursive(active_timestamp);
                                            if peer_edge.left_growth + peer_edge.right_growth == peer_edge.weight {
                                                let peer_is_left = *peer_is_left;
                                                let far_peer_dual_node_internal_ptr: Option<DualNodeInternalPtr> = if peer_is_left {
                                                    peer_edge.right_dual_node.as_ref().map(|ptr| ptr.upgrade_force())
                                                } else {
                                                    peer_edge.left_dual_node.as_ref().map(|ptr| ptr.upgrade_force())
                                                };
                                                match far_peer_dual_node_internal_ptr {
                                                    Some(far_peer_dual_node_internal_ptr) => {
                                                        let far_peer_dual_node_internal = far_peer_dual_node_internal_ptr.read_recursive();
                                                        let far_peer_dual_node_ptr = far_peer_dual_node_internal.origin.upgrade_force();
                                                        if &far_peer_dual_node_ptr != dual_node_ptr {
                                                            let far_peer_dual_node = far_peer_dual_node_ptr.read_recursive();
                                                            match far_peer_dual_node.grow_state {
                                                                DualNodeGrowState::Grow => {
                                                                    let far_peer_grandson_ptr = if peer_is_left {
                                                                        peer_edge.right_grandson_dual_node.as_ref().map(|ptr| ptr.upgrade_force()).unwrap().read_recursive().origin.clone()
                                                                    } else {
                                                                        peer_edge.left_grandson_dual_node.as_ref().map(|ptr| ptr.upgrade_force()).unwrap().read_recursive().origin.clone()
                                                                    };
                                                                    let grandson_ptr = if is_left {
                                                                        edge.left_grandson_dual_node.as_ref().map(|ptr| ptr.upgrade_force()).unwrap().read_recursive().origin.clone()
                                                                    } else {
                                                                        edge.right_grandson_dual_node.as_ref().map(|ptr| ptr.upgrade_force()).unwrap().read_recursive().origin.clone()
                                                                    };
                                                                    return MaxUpdateLength::Conflicting(
                                                                        (far_peer_dual_node_ptr.downgrade(), far_peer_grandson_ptr), 
                                                                        (dual_node_ptr.downgrade(), grandson_ptr)
                                                                    );
                                                                },
                                                                _ => { }
                                                            }
                                                        }
                                                    },
                                                    None => { }
                                                }
                                            }
                                        }
                                    }
                                    continue
                                },
                                DualNodeGrowState::Stay => { remaining_length }
                            };
                            if local_max_length_abs == 0 {
                                let peer_grandson_ptr = if is_left {
                                    edge.right_grandson_dual_node.as_ref().map(|ptr| ptr.upgrade_force()).unwrap().read_recursive().origin.clone()
                                } else {
                                    edge.left_grandson_dual_node.as_ref().map(|ptr| ptr.upgrade_force()).unwrap().read_recursive().origin.clone()
                                };
                                let grandson_ptr = if is_left {
                                    edge.left_grandson_dual_node.as_ref().map(|ptr| ptr.upgrade_force()).unwrap().read_recursive().origin.clone()
                                } else {
                                    edge.right_grandson_dual_node.as_ref().map(|ptr| ptr.upgrade_force()).unwrap().read_recursive().origin.clone()
                                };
                                return MaxUpdateLength::Conflicting(
                                    (peer_dual_node_ptr.downgrade(), peer_grandson_ptr), 
                                    (dual_node_ptr.downgrade(), grandson_ptr)
                                );
                            }
                            max_length_abs = std::cmp::min(max_length_abs, local_max_length_abs);
                        }
                    },
                    None => {
                        let local_max_length_abs = edge.weight - edge.left_growth - edge.right_growth;
                        if local_max_length_abs == 0 {
                            // check if peer is virtual node
                            let peer_vertex_ptr: VertexPtr = if is_left {
                                edge.right.upgrade_force()
                            } else {
                                edge.left.upgrade_force()
                            };
                            let peer_vertex = peer_vertex_ptr.read_recursive(active_timestamp);
                            if peer_vertex.is_virtual || peer_vertex.is_mirror_blocked() {
                                let grandson_ptr = if is_left {
                                    edge.left_grandson_dual_node.as_ref().map(|ptr| ptr.upgrade_force()).unwrap().read_recursive().origin.clone()
                                } else {
                                    edge.right_grandson_dual_node.as_ref().map(|ptr| ptr.upgrade_force()).unwrap().read_recursive().origin.clone()
                                };
                                return MaxUpdateLength::TouchingVirtual((dual_node_ptr.downgrade(), grandson_ptr), peer_vertex.vertex_index);
                            } else {
                                println!("edge: {edge_ptr:?}, peer_vertex_ptr: {peer_vertex_ptr:?}");
                                unreachable!("this edge should've been removed from boundary because it's already fully grown, and it's peer vertex is not virtual")
                            }
                        }
                        max_length_abs = std::cmp::min(max_length_abs, local_max_length_abs);
                    },
                }
            } else {
                if is_left {
                    if edge.left_growth == 0 {
                        unreachable!()
                    }
                    max_length_abs = std::cmp::min(max_length_abs, edge.left_growth);
                } else {
                    if edge.right_growth == 0 {
                        unreachable!()
                    }
                    max_length_abs = std::cmp::min(max_length_abs, edge.right_growth);
                }
            }
        }
        MaxUpdateLength::NonZeroGrow(max_length_abs)
    }

    fn compute_maximum_update_length(&mut self) -> GroupMaxUpdateLength {
        self.renew_active_list();
        // first prepare all nodes for individual grow or shrink; Stay nodes will be prepared to shrink in order to minimize effect on others
        self.prepare_all_growth();
        // after preparing all the growth, there should be no sync requests
        if let Some(unit_module_info) = self.unit_module_info.as_ref() {
            assert!(unit_module_info.sync_requests.is_empty(), "no sync requests should arise here; make sure to deal with all sync requests before growing");
        }
        let mut group_max_update_length = GroupMaxUpdateLength::new();
        for i in 0..self.active_list.len() {
            let dual_node_ptr = {
                let internal_dual_node_ptr = self.active_list[i].upgrade_force();
                let dual_node_internal = internal_dual_node_ptr.read_recursive();
                dual_node_internal.origin.upgrade_force()
            };
            let dual_node = dual_node_ptr.read_recursive();
            let is_grow = match dual_node.grow_state {
                DualNodeGrowState::Grow => true,
                DualNodeGrowState::Shrink => false,
                DualNodeGrowState::Stay => { continue }
            };
            let max_update_length = self.compute_maximum_update_length_dual_node(&dual_node_ptr, is_grow, true);
            group_max_update_length.add(max_update_length);
        }
        group_max_update_length
    }

    fn grow_dual_node(&mut self, dual_node_ptr: &DualNodePtr, length: Weight) {
        let active_timestamp = self.active_timestamp;
        if length == 0 {
            eprintln!("[warning] calling `grow_dual_node` with zero length, nothing to do");
            return
        }
        self.prepare_dual_node_growth(dual_node_ptr, length > 0);
        let dual_node_internal_ptr = self.get_dual_node_internal_ptr(&dual_node_ptr);
        {
            // update node dual variable and do sanity check
            let mut dual_node_internal = dual_node_internal_ptr.write();
            dual_node_internal.dual_variable += length;
            assert!(dual_node_internal.dual_variable >= 0, "shrinking to negative dual variable is forbidden");
            // update over-grown vertices
            if dual_node_internal.overgrown_stack.len() > 0 {
                let last_index = dual_node_internal.overgrown_stack.len() - 1;
                let (_, overgrown) = &mut dual_node_internal.overgrown_stack[last_index];
                if length < 0 { assert!( *overgrown >= -length, "overgrown vertex cannot shrink so much"); }
                *overgrown += length;
            }
        }
        let dual_node_internal = dual_node_internal_ptr.read_recursive();
        for (is_left, edge_weak) in dual_node_internal.boundary.iter() {
            let edge_ptr = edge_weak.upgrade_force();
            let is_left = *is_left;
            let (growth, weight) = {  // minimize writer lock acquisition
                let mut edge = edge_ptr.write(active_timestamp);
                if is_left {
                    edge.left_growth += length;
                    assert!(edge.left_growth >= 0, "negative growth forbidden");
                } else {
                    edge.right_growth += length;
                    assert!(edge.right_growth >= 0, "negative growth forbidden");
                }
                (edge.left_growth + edge.right_growth, edge.weight)
            };
            let edge = edge_ptr.read_recursive(active_timestamp);
            if growth > weight {
                // first check for if both side belongs to the same dual node, if so, it's ok
                let dual_node_internal_ptr_2: &Option<DualNodeInternalWeak> = if is_left {
                    &edge.right_dual_node
                } else {
                    &edge.left_dual_node
                };
                if dual_node_internal_ptr_2.is_none() || dual_node_internal_ptr_2.as_ref().unwrap() != &dual_node_internal_ptr.downgrade() {
                    let left_ptr = edge.left.upgrade_force();
                    let right_ptr = edge.right.upgrade_force();
                    panic!("over-grown edge ({},{}): {}/{}", left_ptr.read_recursive(active_timestamp).vertex_index
                        , right_ptr.read_recursive(active_timestamp).vertex_index, growth, weight);
                }
            } else if growth < 0 {
                let left_ptr = edge.left.upgrade_force();
                let right_ptr = edge.right.upgrade_force();
                panic!("under-grown edge ({},{}): {}/{}", left_ptr.read_recursive(active_timestamp).vertex_index
                    , right_ptr.read_recursive(active_timestamp).vertex_index, growth, weight);
            }
        }
    }

    fn grow(&mut self, length: Weight) {
        assert!(length > 0, "only positive growth is supported");
        self.renew_active_list();
        // first handle shrinks and then grow, to make sure they don't conflict
        for i in 0..self.active_list.len() {
            let dual_node_ptr = {
                let internal_dual_node_ptr = self.active_list[i].upgrade_force();
                let dual_node_internal = internal_dual_node_ptr.read_recursive();
                dual_node_internal.origin.upgrade_force()
            };
            let dual_node = dual_node_ptr.read_recursive();
            if matches!(dual_node.grow_state, DualNodeGrowState::Shrink) {
                self.grow_dual_node(&dual_node_ptr, -length);
            }
        }
        // then grow those needed
        for i in 0..self.active_list.len() {
            let dual_node_ptr = {
                let internal_dual_node_ptr = self.active_list[i].upgrade_force();
                let dual_node_internal = internal_dual_node_ptr.read_recursive();
                dual_node_internal.origin.upgrade_force()
            };
            let dual_node = dual_node_ptr.read_recursive();
            if matches!(dual_node.grow_state, DualNodeGrowState::Grow) {
                self.grow_dual_node(&dual_node_ptr, length);
            }
        }
    }

    fn load_edge_modifier(&mut self, edge_modifier: &Vec<(EdgeIndex, Weight)>) {
        assert!(!self.edge_modifier.has_modified_edges(), "the current erasure modifier is not clean, probably forget to clean the state?");
        let active_timestamp = self.active_timestamp;
        for (edge_index, target_weight) in edge_modifier.iter() {
            let edge_ptr = &self.edges[*edge_index];
            edge_ptr.dynamic_clear(active_timestamp);  // may visit stale edges
            let mut edge = edge_ptr.write(active_timestamp);
            let original_weight = edge.weight;
            edge.weight = *target_weight;
            self.edge_modifier.push_modified_edge(*edge_index, original_weight);
        }
    }

}

/*
Implementing fast clear operations
*/

impl FastClear for Edge {

    fn hard_clear(&mut self) {
        self.left_growth = 0;
        self.right_growth = 0;
        self.left_dual_node = None;
        self.left_grandson_dual_node = None;
        self.right_dual_node = None;
        self.right_grandson_dual_node = None;
    }

    fn get_timestamp(&self) -> FastClearTimestamp { self.timestamp }
    fn set_timestamp(&mut self, timestamp: FastClearTimestamp) { self.timestamp = timestamp; }

}

impl FastClear for Vertex {

    fn hard_clear(&mut self) {
        self.is_syndrome = false;
        self.propagated_dual_node = None;
        self.propagated_grandson_dual_node = None;
    }

    fn get_timestamp(&self) -> FastClearTimestamp { self.timestamp }
    fn set_timestamp(&mut self, timestamp: FastClearTimestamp) { self.timestamp = timestamp; }

}

impl Vertex {

    /// if this vertex is a mirrored vertex and it's disabled, it can be temporarily matched just like a virtual vertex
    pub fn is_mirror_blocked(&self) -> bool {
        if let Some(ref mirror_unit_ptr) = self.mirror_unit {
            let mirror_unit_ptr = mirror_unit_ptr.upgrade_force();
            let mirror_unit = mirror_unit_ptr.read_recursive();
            !mirror_unit.enabled
        } else {
            false
        }
    }

}

impl DualModuleSerial {

    /// create a partitioned serial dual module to be used in the parallel dual module
    pub fn new_partitioned(partitioned_initializer: &PartitionedSolverInitializer) -> Self {
        println!("partitioned_initializer: {partitioned_initializer:?}");
        let active_timestamp = 0;
        // create vertices
        let mut vertices: Vec<VertexPtr> = partitioned_initializer.owning_range.iter().map(|vertex_index| VertexPtr::new(Vertex {
            vertex_index: vertex_index,
            is_virtual: false,
            is_syndrome: false,
            mirror_unit: partitioned_initializer.owning_interface.clone(),
            edges: Vec::new(),
            propagated_dual_node: None,
            propagated_grandson_dual_node: None,
            timestamp: active_timestamp,
        })).collect();
        // set virtual vertices
        for &virtual_vertex in partitioned_initializer.virtual_vertices.iter() {
            let mut vertex = vertices[virtual_vertex - partitioned_initializer.owning_range.start()].write(active_timestamp);
            vertex.is_virtual = true;
        }
        // add interface vertices
        let mut mirrored_vertices = HashMap::<VertexIndex, usize>::new();  // all mirrored vertices mapping to their local indices
        for (mirror_unit, interface_vertices) in partitioned_initializer.interfaces.iter() {
            for (vertex_index, is_virtual) in interface_vertices.iter() {
                mirrored_vertices.insert(*vertex_index, vertices.len());
                vertices.push(VertexPtr::new(Vertex {
                    vertex_index: *vertex_index,
                    is_virtual: *is_virtual,  // interface vertices are always virtual at the beginning
                    is_syndrome: false,
                    mirror_unit: Some(mirror_unit.clone()),
                    edges: Vec::new(),
                    propagated_dual_node: None,
                    propagated_grandson_dual_node: None,
                    timestamp: active_timestamp,
                }))
            }
        }
        // set edges
        let mut edges = Vec::<EdgePtr>::new();
        for &(i, j, weight, edge_index) in partitioned_initializer.weighted_edges.iter() {
            assert_ne!(i, j, "invalid edge from and to the same vertex {}", i);
            assert!(partitioned_initializer.owning_range.contains(&i) || mirrored_vertices.contains_key(&i), "edge ({}, {}) connected to an invalid vertex {}", i, j, i);
            assert!(partitioned_initializer.owning_range.contains(&j) || mirrored_vertices.contains_key(&j), "edge ({}, {}) connected to an invalid vertex {}", i, j, j);
            let left = usize::min(i, j);
            let right = usize::max(i, j);
            let left_index = if partitioned_initializer.owning_range.contains(&left) {
                left - partitioned_initializer.owning_range.start()
            } else {
                mirrored_vertices[&left]
            };
            let right_index = if partitioned_initializer.owning_range.contains(&right) {
                right - partitioned_initializer.owning_range.start()
            } else {
                mirrored_vertices[&right]
            };
            let edge_ptr = EdgePtr::new(Edge {
                edge_index: edge_index,
                weight: weight,
                left: vertices[left_index].downgrade(),
                right: vertices[right_index].downgrade(),
                left_growth: 0,
                right_growth: 0,
                left_dual_node: None,
                left_grandson_dual_node: None,
                right_dual_node: None,
                right_grandson_dual_node: None,
                timestamp: 0,
            });
            for (a, b) in [(left_index, right_index), (right_index, left_index)] {
                let mut vertex = vertices[a].write(active_timestamp);
                debug_assert!({  // O(N^2) sanity check, debug mode only (actually this bug is not critical, only the shorter edge will take effect)
                    let mut no_duplicate = true;
                    for edge_weak in vertex.edges.iter() {
                        let edge_ptr = edge_weak.upgrade_force();
                        let edge = edge_ptr.read_recursive(active_timestamp);
                        if edge.left == vertices[b].downgrade() || edge.right == vertices[b].downgrade() {
                            no_duplicate = false;
                            eprintln!("duplicated edge between {} and {} with weight w1 = {} and w2 = {}, consider merge them into a single edge", i, j, weight, edge.weight);
                            break
                        }
                    }
                    no_duplicate
                });
                vertex.edges.push(edge_ptr.downgrade());
            }
            edges.push(edge_ptr);
        }
        Self {
            vertices: vertices,
            nodes: vec![],
            edges: edges,
            active_timestamp: 0,
            vertex_num: partitioned_initializer.vertex_num,
            edge_num: partitioned_initializer.edge_num,
            owning_range: partitioned_initializer.owning_range,
            unit_module_info: Some(UnitModuleInfo {
                unit_index: partitioned_initializer.unit_index,
                mirrored_vertices: mirrored_vertices,
                owning_dual_range: VertexRange::new(0, 0),
                dual_node_pointers: PtrWeakKeyHashMap::<DualNodeWeak, usize>::new(),
                sync_requests: vec![],
            }),
            active_list: vec![],
            current_cycle: 0,
            edge_modifier: EdgeWeightModifier::new(),
        }
    }

    pub fn prepare_all_growth(&mut self) {
        for i in 0..self.active_list.len() {
            let dual_node_ptr = {
                let internal_dual_node_ptr = self.active_list[i].upgrade_force();
                let dual_node_internal = internal_dual_node_ptr.read_recursive();
                dual_node_internal.origin.upgrade_force()
            };
            let dual_node = dual_node_ptr.read_recursive();
            match dual_node.grow_state {
                DualNodeGrowState::Grow => { },
                DualNodeGrowState::Shrink => { self.prepare_dual_node_growth(&dual_node_ptr, false); },
                DualNodeGrowState::Stay => { },  // do not touch, Stay nodes might have become a part of a blossom, so it's not safe to change the boundary
            };
        }
        for i in 0..self.active_list.len() {
            let dual_node_ptr = {
                let internal_dual_node_ptr = self.active_list[i].upgrade_force();
                let dual_node_internal = internal_dual_node_ptr.read_recursive();
                dual_node_internal.origin.upgrade_force()
            };
            let dual_node = dual_node_ptr.read_recursive();
            match dual_node.grow_state {
                DualNodeGrowState::Grow => { self.prepare_dual_node_growth(&dual_node_ptr, true); },
                DualNodeGrowState::Shrink => { },
                DualNodeGrowState::Stay => { },  // do not touch, Stay nodes might have become a part of a blossom, so it's not safe to change the boundary
            };
        }
    }

    /// before creating a blossom, all nodes must be prepared to shrink throughout all partitioned dual modules
    pub fn prepare_blossom_initial_shrink(&mut self, nodes_circle: &Vec<DualNodePtr>) {
        for dual_node_ptr in nodes_circle.iter() {
            if self.contains_dual_node(dual_node_ptr) {
                self.prepare_dual_node_growth(&dual_node_ptr, false);  // prepare to shrink
            }
        }
    }

    /// hard clear all growth (manual call not recommended due to performance drawback)
    pub fn hard_clear_graph(&mut self) {
        for edge in self.edges.iter() {
            let mut edge = edge.write_force();
            edge.hard_clear();
            edge.timestamp = 0;
        }
        for vertex in self.vertices.iter() {
            let mut vertex = vertex.write_force();
            vertex.hard_clear();
            vertex.timestamp = 0;
        }
        self.active_timestamp = 0;
    }

    /// soft clear all growth
    pub fn clear_graph(&mut self) {
        if self.active_timestamp == FastClearTimestamp::MAX {  // rarely happens
            self.hard_clear_graph();
        }
        self.active_timestamp += 1;  // implicitly clear all edges growth
    }

    /// increment the global cycle so that each node in the active list can be accessed exactly once
    fn renew_active_list(&mut self) {
        if self.current_cycle == usize::MAX {
            for i in 0..self.nodes.len() {
                let internal_dual_node_ptr = {
                    match self.nodes[i].as_ref() {
                        Some(internal_dual_node_ptr) => {
                            internal_dual_node_ptr.clone()
                        },
                        _ => { continue }
                    }
                };
                let mut internal_dual_node = internal_dual_node_ptr.write();
                internal_dual_node.last_visit_cycle = 0;
            }
            self.current_cycle = 0;
        }
        self.current_cycle += 1;
        // renew the active_list
        let mut updated_active_list = Vec::with_capacity(self.active_list.len());
        for i in 0..self.active_list.len() {
            let (dual_node_ptr, internal_dual_node_ptr) = {
                match self.active_list[i].upgrade() {
                    Some(internal_dual_node_ptr) => {
                        let mut dual_node_internal = internal_dual_node_ptr.write();
                        if self.nodes[dual_node_internal.index].is_none() { continue }  // removed
                        if dual_node_internal.last_visit_cycle == self.current_cycle { continue }  // visited
                        dual_node_internal.last_visit_cycle = self.current_cycle;  // mark as visited
                        (dual_node_internal.origin.upgrade_force(), internal_dual_node_ptr.clone())
                    },
                    _ => { continue }
                }
            };
            let dual_node = dual_node_ptr.read_recursive();
            match dual_node.grow_state {
                DualNodeGrowState::Grow | DualNodeGrowState::Shrink => {
                    updated_active_list.push(internal_dual_node_ptr.downgrade());
                }, DualNodeGrowState::Stay => {

                },  // no longer in the active list
            };
        }
        self.active_list = updated_active_list;
    }

    fn sanity_check_grandson(&self, propagated_dual_node_weak: &DualNodeInternalWeak, propagated_grandson_dual_node_weak: &DualNodeInternalWeak) -> Result<(), String> {
        let propagated_dual_node_ptr = propagated_dual_node_weak.upgrade_force();
        let propagated_grandson_dual_node_ptr = propagated_grandson_dual_node_weak.upgrade_force();
        let propagated_dual_node = propagated_dual_node_ptr.read_recursive();
        let propagated_grandson_dual_node = propagated_grandson_dual_node_ptr.read_recursive();
        let propagated_node_ptr = propagated_dual_node.origin.upgrade_force();
        let propagated_node = propagated_node_ptr.read_recursive();
        let propagated_grandson_ptr = propagated_grandson_dual_node.origin.upgrade_force();
        let propagated_grandson = propagated_grandson_ptr.read_recursive();
        if matches!(propagated_grandson.class, DualNodeClass::SyndromeVertex{..}) {
            if matches!(propagated_node.class, DualNodeClass::SyndromeVertex{..}) {
                if propagated_dual_node_ptr != propagated_grandson_dual_node_ptr {
                    return Err(format!("syndrome node {:?} must have grandson equal to itself {:?}", propagated_dual_node_ptr, propagated_grandson_dual_node_ptr))
                }
            } else {
                // test if grandson is a real grandson
                drop(propagated_grandson);
                let mut descendant_ptr = propagated_grandson_ptr;
                loop {
                    let descendant = descendant_ptr.read_recursive();
                    if let Some(descendant_parent_ptr) = descendant.parent_blossom.as_ref() {
                        let descendant_parent_ptr = descendant_parent_ptr.upgrade_force();
                        if descendant_parent_ptr == propagated_node_ptr {
                            return Ok(())
                        }
                        drop(descendant);
                        descendant_ptr = descendant_parent_ptr;
                    } else {
                        return Err(format!("grandson check failed"));
                    }
                }
            }
        } else {
            return Err(format!("grandson must be a vertex"))
        }
        Ok(())
    }

    /// do a sanity check of if all the nodes are in consistent state
    pub fn sanity_check(&self) -> Result<(), String> {
        let active_timestamp = self.active_timestamp;
        for vertex_ptr in self.vertices.iter() {
            vertex_ptr.dynamic_clear(active_timestamp);
            let vertex = vertex_ptr.read_recursive(active_timestamp);
            if let Some(propagated_grandson_dual_node) = vertex.propagated_grandson_dual_node.as_ref() {
                if let Some(propagated_dual_node) = vertex.propagated_dual_node.as_ref() {
                    self.sanity_check_grandson(propagated_dual_node, propagated_grandson_dual_node)?;
                } else {
                    return Err(format!("vertex {} has propagated grandson dual node {:?} but missing propagated dual node"
                        , vertex.vertex_index, vertex.propagated_grandson_dual_node))
                }
            }
            if vertex.propagated_dual_node.is_some() && vertex.propagated_grandson_dual_node.is_none() {
                return Err(format!("vertex {} has propagated dual node {:?} but missing grandson", vertex.vertex_index, vertex.propagated_dual_node))
            }
        }
        // sanity check that boundary doesn't include duplicate edges
        let mut duplicate_edges = HashMap::<(bool, EdgeIndex), NodeIndex>::new();
        for (node_index, node_ptr) in self.nodes.iter().enumerate() {
            if let Some(node_ptr) = node_ptr {
                let dual_node_internal = node_ptr.read_recursive();
                if dual_node_internal.origin.upgrade_force().read_recursive().parent_blossom.is_some() {
                    continue  // skip this node, since it's inactive
                }
                for (is_left, edge_weak) in dual_node_internal.boundary.iter() {
                    let edge_ptr = edge_weak.upgrade_force();
                    let edge = edge_ptr.read_recursive(active_timestamp);
                    if duplicate_edges.contains_key(&(*is_left, edge.edge_index)) {
                        return Err(format!("boundary edge {:?} appears twice in node {} and {}", (*is_left, edge.edge_index), duplicate_edges.get(&(*is_left, edge.edge_index)).unwrap(), node_index));
                    }
                    duplicate_edges.insert((*is_left, edge.edge_index), node_index);
                }
            }
        }
        Ok(())
    }

}

/*
Implementing visualization functions
*/

impl FusionVisualizer for DualModuleSerial {
    fn snapshot(&self, abbrev: bool) -> serde_json::Value {
        // do the sanity check first before taking snapshot
        self.sanity_check().unwrap();
        let active_timestamp = self.active_timestamp;
        let mut vertices: Vec::<serde_json::Value> = (0..self.vertex_num).map(|_| serde_json::Value::Null).collect();
        for vertex_ptr in self.vertices.iter() {
            vertex_ptr.dynamic_clear(active_timestamp);
            let vertex = vertex_ptr.read_recursive(active_timestamp);
            vertices[vertex.vertex_index] = json!({
                if abbrev { "v" } else { "is_virtual" }: if vertex.is_virtual { 1 } else { 0 },
                if abbrev { "s" } else { "is_syndrome" }: if vertex.is_syndrome { 1 } else { 0 },
            });
            if let Some(value) = vertex.propagated_dual_node.as_ref().map(|weak| weak.upgrade_force().read_recursive().origin.upgrade_force().read_recursive().index) {
                vertices[vertex.vertex_index].as_object_mut().unwrap().insert((if abbrev { "p" } else { "propagated_dual_node" }).to_string(), json!(value));
            }
            if let Some(value) = vertex.propagated_grandson_dual_node.as_ref().map(|weak| weak.upgrade_force().read_recursive().origin.upgrade_force().read_recursive().index) {
                vertices[vertex.vertex_index].as_object_mut().unwrap().insert((if abbrev { "pg" } else { "propagated_grandson_dual_node" }).to_string(), json!(value));
            }
            if let Some(mirror_unit_ptr) = vertex.mirror_unit.as_ref() {
                let mirror_unit_ptr = mirror_unit_ptr.upgrade_force();
                let mirror_unit = mirror_unit_ptr.read_recursive();
                vertices[vertex.vertex_index].as_object_mut().unwrap().insert((if abbrev { "mi" } else { "mirror_unit_index" }).to_string()
                    , json!(mirror_unit.unit_index));
                vertices[vertex.vertex_index].as_object_mut().unwrap().insert((if abbrev { "me" } else { "mirror_enabled" }).to_string()
                    , json!(if mirror_unit.enabled { 1 } else { 0 }));
            }
        }
        let mut edges: Vec::<serde_json::Value> = (0..self.edge_num).map(|_| serde_json::Value::Null).collect();
        for edge_ptr in self.edges.iter() {
            edge_ptr.dynamic_clear(active_timestamp);
            let edge = edge_ptr.read_recursive(active_timestamp);
            edges[edge.edge_index] = json!({
                if abbrev { "w" } else { "weight" }: edge.weight,
                if abbrev { "l" } else { "left" }: edge.left.upgrade_force().read_recursive(active_timestamp).vertex_index,
                if abbrev { "r" } else { "right" }: edge.right.upgrade_force().read_recursive(active_timestamp).vertex_index,
                if abbrev { "lg" } else { "left_growth" }: edge.left_growth,
                if abbrev { "rg" } else { "right_growth" }: edge.right_growth,
            });
            if let Some(value) = edge.left_dual_node.as_ref().map(|weak| weak.upgrade_force().read_recursive().origin.upgrade_force().read_recursive().index) {
                edges[edge.edge_index].as_object_mut().unwrap().insert((if abbrev { "ld" } else { "left_dual_node" }).to_string(), json!(value));
            }
            if let Some(value) = edge.left_grandson_dual_node.as_ref().map(|weak| weak.upgrade_force().read_recursive().origin.upgrade_force().read_recursive().index) {
                edges[edge.edge_index].as_object_mut().unwrap().insert((if abbrev { "lgd" } else { "left_grandson_dual_node" }).to_string(), json!(value));
            }
            if let Some(value) = edge.right_dual_node.as_ref().map(|weak| weak.upgrade_force().read_recursive().origin.upgrade_force().read_recursive().index) {
                edges[edge.edge_index].as_object_mut().unwrap().insert((if abbrev { "rd" } else { "right_dual_node" }).to_string(), json!(value));
            }
            if let Some(value) = edge.right_grandson_dual_node.as_ref().map(|weak| weak.upgrade_force().read_recursive().origin.upgrade_force().read_recursive().index) {
                edges[edge.edge_index].as_object_mut().unwrap().insert((if abbrev { "rgd" } else { "right_grandson_dual_node" }).to_string(), json!(value));
            }
            
        }
        let mut value = json!({
            "vertices": vertices,
            "edges": edges,
        });
        // TODO: since each serial module only processes a part of the dual nodes, it's not feasible to list them in a reasonable vector now...
        if self.owning_range.start() == 0 && self.owning_range.end() == self.vertex_num {
            let mut dual_nodes = Vec::<serde_json::Value>::new();
            for node_ptr in self.nodes.iter() {
                if let Some(node_ptr) = node_ptr.as_ref() {
                    let node = node_ptr.read_recursive();
                    dual_nodes.push(json!({
                        if abbrev { "b" } else { "boundary" }: node.boundary.iter().map(|(is_left, edge_weak)|
                            (*is_left, edge_weak.upgrade_force().read_recursive(active_timestamp).edge_index)).collect::<Vec<(bool, usize)>>(),
                        if abbrev { "d" } else { "dual_variable" }: node.dual_variable,
                    }));
                } else {
                    dual_nodes.push(json!(null));
                }
            }
            value.as_object_mut().unwrap().insert(format!("dual_nodes"), json!(dual_nodes));
        }
        value
    }
}

/*
Implement internal helper functions that maintains the state of dual clusters
*/

impl DualModuleSerial {

    /// register a new dual node ptr, but not creating the internal dual node
    fn register_dual_node_ptr(&mut self, dual_node_ptr: &DualNodePtr) {
        // println!("unit {:?}, register_dual_node_ptr: {:?}", self.unit_module_info, dual_node_ptr);
        let node = dual_node_ptr.read_recursive();
        if let Some(unit_module_info) = self.unit_module_info.as_mut() {
            if unit_module_info.owning_dual_range.len() == 0 {  // set the range instead of inserting into the lookup table, to minimize table lookup
                unit_module_info.owning_dual_range = VertexRange::new(node.index, node.index);
            }
            if unit_module_info.owning_dual_range.end() == node.index && self.nodes.len() == unit_module_info.owning_dual_range.len() {
                // it's able to append into the owning range, minimizing table lookup and thus better performance
                unit_module_info.owning_dual_range.append_by(1);
            } else {
                // will be inserted at this place
                unit_module_info.dual_node_pointers.insert(dual_node_ptr.clone(), self.nodes.len());
            }
        } else {
            assert!(self.nodes.len() == node.index, "dual node must be created in a sequential manner: no missing or duplicating");
        }
        // println!("unit {:?}, register_dual_node_ptr: {:?}", self.unit_module_info, dual_node_ptr);
    }

    pub fn get_dual_node_index(&self, dual_node_ptr: &DualNodePtr) -> Option<usize> {
        let dual_node = dual_node_ptr.read_recursive();
        if let Some(unit_module_info) = self.unit_module_info.as_ref() {
            if unit_module_info.owning_dual_range.contains(&dual_node.index) {
                Some(dual_node.index - unit_module_info.owning_dual_range.start())
            } else {
                // println!("from unit {:?}, dual_node: {}", self.unit_module_info, dual_node.index);
                unit_module_info.dual_node_pointers.get(dual_node_ptr).map(|x| *x)
            }
        } else {
            Some(dual_node.index)
        }
    }

    pub fn contains_dual_node(&self, dual_node_ptr: &DualNodePtr) -> bool {
        self.get_dual_node_index(dual_node_ptr).is_some()
    }

    pub fn contains_dual_nodes_any(&self, dual_node_ptrs: &Vec<DualNodePtr>) -> bool {
        for dual_node_ptr in dual_node_ptrs.iter() {
            if self.contains_dual_node(dual_node_ptr) {
                return true
            }
        }
        false
    }

    pub fn get_vertex_index(&self, vertex_index: VertexIndex) -> Option<usize> {
        if self.owning_range.contains(&vertex_index) {
            return Some(vertex_index - self.owning_range.start())
        }
        if let Some(unit_module_info) = self.unit_module_info.as_ref() {
            if let Some(index) = unit_module_info.mirrored_vertices.get(&vertex_index) {
                return Some(*index)
            }
        }
        None
    }

    pub fn contains_vertex(&self, vertex_index: VertexIndex) -> bool {
        self.get_vertex_index(vertex_index).is_some()
    }

    pub fn get_dual_node_internal_ptr(&self, dual_node_ptr: &DualNodePtr) -> DualNodeInternalPtr {
        self.get_dual_node_internal_ptr_optional(dual_node_ptr).unwrap()
    }

    // dual node ptr may not hold in this module
    pub fn get_dual_node_internal_ptr_optional(&self, dual_node_ptr: &DualNodePtr) -> Option<DualNodeInternalPtr> {
        self.get_dual_node_index(dual_node_ptr).map(|dual_node_index| {
            let dual_node_internal_ptr = self.nodes[dual_node_index].as_ref().expect("internal dual node must exists");
            debug_assert!(dual_node_ptr == &dual_node_internal_ptr.read_recursive().origin.upgrade_force(), "dual node and dual internal node must corresponds to each other");
            dual_node_internal_ptr.clone()
        })
    }

    pub fn get_otherwise_add_dual_node(&mut self, dual_node_ptr: &DualNodePtr) -> DualNodeInternalPtr {
        let dual_node_index = self.get_dual_node_index(&dual_node_ptr).unwrap_or_else(|| {
            // add a new internal dual node corresponding to the dual_node_ptr
            self.register_dual_node_ptr(dual_node_ptr);
            let dual_node_index = self.nodes.len();
            let node_internal_ptr = DualNodeInternalPtr::new(DualNodeInternal {
                origin: dual_node_ptr.downgrade(),
                index: dual_node_index,
                dual_variable: 0,  // TODO: need dual variable
                boundary: Vec::new(),
                overgrown_stack: Vec::new(),
                last_visit_cycle: 0,
            });
            self.active_list.push(node_internal_ptr.downgrade());
            self.nodes.push(Some(node_internal_ptr));
            dual_node_index
        });
        let dual_node_internal_ptr = self.nodes[dual_node_index].as_ref().expect("internal dual node must exists");
        debug_assert!(dual_node_ptr == &dual_node_internal_ptr.read_recursive().origin.upgrade_force(), "dual node and dual internal node must corresponds to each other");
        dual_node_internal_ptr.clone()
    }

    /// execute a synchronize event by updating the state of a vertex and also update the internal dual node accordingly
    pub fn execute_sync_event(&mut self, sync_event: &SyncRequest) {
        let active_timestamp = self.active_timestamp;
        debug_assert!(self.contains_vertex(sync_event.vertex_index));
        let propagated_dual_node_internal_ptr = sync_event.propagated_dual_node.as_ref().map(|dual_node_weak| {
            self.get_otherwise_add_dual_node(&dual_node_weak.upgrade_force())
        });
        let propagated_grandson_dual_node_internal_ptr = sync_event.propagated_grandson_dual_node.as_ref().map(|dual_node_weak| {
            self.get_otherwise_add_dual_node(&dual_node_weak.upgrade_force())
        });
        let local_vertex_index = self.get_vertex_index(sync_event.vertex_index).expect("cannot synchronize at a non-existing vertex");
        let vertex_ptr = &self.vertices[local_vertex_index];
        vertex_ptr.dynamic_clear(active_timestamp);
        let mut vertex = vertex_ptr.write(active_timestamp);
        if vertex.propagated_dual_node == propagated_dual_node_internal_ptr.as_ref().map(|x| x.downgrade()) {
            // actually this may happen: if the same vertex is propagated from two different units with the same distance
            // to the closest grandson, it may happen that sync event will conflict on the grandson...
            // this conflict doesn't matter anyway: any grandson is good
            // assert_eq!(vertex.propagated_grandson_dual_node, propagated_grandson_dual_node_internal_ptr.as_ref().map(|x| x.downgrade()));
            vertex.propagated_grandson_dual_node = propagated_grandson_dual_node_internal_ptr.as_ref().map(|x| x.downgrade());
        } else {  // conflict with existing value, action needed
            // first vacate the vertex, recovering dual node boundaries accordingly
            if let Some(dual_node_internal_weak) = vertex.propagated_dual_node.as_ref() {
                assert!(!vertex.is_syndrome, "cannot vacate a syndrome vertex: it shouldn't happen that a syndrome vertex is updated in any partitioned unit");
                let mut updated_boundary = Vec::<(bool, EdgeWeak)>::new();
                let dual_node_internal_ptr = dual_node_internal_weak.upgrade_force();
                let mut dual_node_internal = dual_node_internal_ptr.write();
                vertex.propagated_dual_node = None;
                vertex.propagated_grandson_dual_node = None;
                // iterate over the boundary to remove any edges associated with the vertex and also reset those edges
                for (is_left, edge_weak) in dual_node_internal.boundary.iter() {
                    let is_left = *is_left;
                    let edge_ptr = edge_weak.upgrade_force();
                    let mut edge = edge_ptr.write(active_timestamp);
                    let this_vertex_ptr = if is_left {
                        edge.left.upgrade_force()
                    } else {
                        edge.right.upgrade_force()
                    };
                    if &this_vertex_ptr == vertex_ptr {
                        debug_assert!(if is_left { edge.left_growth == 0 } else { edge.right_growth == 0 }, "vacating a non-boundary vertex is forbidden");
                        let dual_node = if is_left { &edge.left_dual_node } else { &edge.right_dual_node };
                        if let Some(dual_node_weak) = dual_node.as_ref() {
                            // sanity check: if exists, must be the same
                            debug_assert!(dual_node_weak.upgrade_force() == dual_node_internal_ptr);
                            // reset the dual node to be unoccupied
                            let dual_node = if is_left { &mut edge.left_dual_node } else { &mut edge.right_dual_node };
                            *dual_node = None;
                            let grandson_dual_node = if is_left { &mut edge.left_grandson_dual_node } else { &mut edge.right_grandson_dual_node };
                            *grandson_dual_node = None;
                        }
                    } else {
                        updated_boundary.push((is_left, edge_weak.clone()));
                    }
                }
                // iterate over the edges around the vertex to add edges to the boundary
                for edge_weak in vertex.edges.iter() {
                    let edge_ptr = edge_weak.upgrade_force();
                    edge_ptr.dynamic_clear(active_timestamp);
                    let mut edge = edge_ptr.write(active_timestamp);
                    let is_left = vertex_ptr.downgrade() == edge.left;
                    let dual_node = if is_left { &edge.left_dual_node } else { &edge.right_dual_node };
                    if let Some(dual_node_weak) = dual_node.as_ref() {
                        // sanity check: if exists, must be the same
                        debug_assert!(dual_node_weak.upgrade_force() == dual_node_internal_ptr);
                        // need to add to the boundary
                        let dual_node = if is_left { &mut edge.left_dual_node } else { &mut edge.right_dual_node };
                        *dual_node = None;
                        let grandson_dual_node = if is_left { &mut edge.left_grandson_dual_node } else { &mut edge.right_grandson_dual_node };
                        *grandson_dual_node = None;
                        updated_boundary.push((!is_left, edge_weak.clone()));
                    }
                }
                // update the boundary
                std::mem::swap(&mut updated_boundary, &mut dual_node_internal.boundary);
            }
            // then update the vertex to the dual node
            if let Some(dual_node_internal_ptr) = propagated_dual_node_internal_ptr.as_ref() {
                // grandson dual node must present
                let grandson_dual_node_internal_ptr = propagated_grandson_dual_node_internal_ptr.unwrap();
                vertex.propagated_dual_node = Some(dual_node_internal_ptr.downgrade());
                vertex.propagated_grandson_dual_node = Some(grandson_dual_node_internal_ptr.downgrade());
                let mut dual_node_internal = dual_node_internal_ptr.write();
                for edge_weak in vertex.edges.iter() {
                    let edge_ptr = edge_weak.upgrade_force();
                    edge_ptr.dynamic_clear(active_timestamp);
                    let mut edge = edge_ptr.write(active_timestamp);
                    let is_left = vertex_ptr.downgrade() == edge.left;
                    let dual_node = if is_left { &mut edge.left_dual_node } else { &mut edge.right_dual_node };
                    assert_eq!(dual_node, &None, "edges incident to the vertex must have been vacated");
                    *dual_node = Some(dual_node_internal_ptr.downgrade());
                    let grandson_dual_node = if is_left { &mut edge.left_grandson_dual_node } else { &mut edge.right_grandson_dual_node };
                    *grandson_dual_node = Some(grandson_dual_node_internal_ptr.downgrade());
                    dual_node_internal.boundary.push((is_left, edge_weak.clone()));
                }
            }
            
        }
    }

    /// this is equivalent to [`DualModuleSerial::prepare_dual_node_growth`] when there are no 0 weight edges, but when it encounters zero-weight edges, it will report `true`
    pub fn prepare_dual_node_growth_single(&mut self, dual_node_ptr: &DualNodePtr, is_grow: bool) -> bool {
        let active_timestamp = self.active_timestamp;
        let mut updated_boundary = Vec::<(bool, EdgeWeak)>::new();
        let mut propagating_vertices = Vec::<(VertexPtr, Option<DualNodeInternalWeak>)>::new();
        let dual_node_internal_ptr = self.get_dual_node_internal_ptr(&dual_node_ptr);
        let mut newly_propagated_edge_has_zero_weight = false;
        if is_grow {  // gracefully update the boundary to ease growing
            let dual_node_internal = dual_node_internal_ptr.read_recursive();
            for (is_left, edge_weak) in dual_node_internal.boundary.iter() {
                let edge_ptr = edge_weak.upgrade_force();
                let is_left = *is_left;
                let edge = edge_ptr.read_recursive(active_timestamp);
                let peer_dual_node: &Option<DualNodeInternalWeak> = if is_left {
                    &edge.right_dual_node
                } else {
                    &edge.left_dual_node
                };
                if edge.left_growth + edge.right_growth == edge.weight && peer_dual_node.is_none() {
                    // need to propagate to a new node
                    let peer_vertex_ptr = if is_left {
                        edge.right.upgrade_force()
                    } else {
                        edge.left.upgrade_force()
                    };
                    // to avoid already occupied node being propagated
                    peer_vertex_ptr.dynamic_clear(active_timestamp);
                    let peer_vertex = peer_vertex_ptr.read_recursive(active_timestamp);
                    if peer_vertex.is_virtual || peer_vertex.is_mirror_blocked() {  // virtual node is never propagated, so keep this edge in the boundary
                        updated_boundary.push((is_left, edge_weak.clone()));
                    } else {
                        assert!(peer_vertex.propagated_dual_node.is_none(), "growing into another propagated vertex forbidden");
                        assert!(peer_vertex.propagated_grandson_dual_node.is_none(), "growing into another propagated vertex forbidden");
                        propagating_vertices.push((peer_vertex_ptr.clone(), if is_left { edge.left_grandson_dual_node.clone() } else { edge.right_grandson_dual_node.clone() }));
                        // this edge is dropped, so we need to set both end of this edge to this dual node
                        drop(edge);  // unlock read
                        let mut edge = edge_ptr.write(active_timestamp);
                        if is_left {
                            edge.right_dual_node = Some(dual_node_internal_ptr.downgrade());
                            debug_assert!(edge.left_grandson_dual_node.is_some());
                            edge.right_grandson_dual_node = edge.left_grandson_dual_node.clone();
                        } else {
                            edge.left_dual_node = Some(dual_node_internal_ptr.downgrade());
                            debug_assert!(edge.right_grandson_dual_node.is_some());
                            edge.left_grandson_dual_node = edge.right_grandson_dual_node.clone();
                        }
                    }
                } else {  // keep other edges
                    updated_boundary.push((is_left, edge_weak.clone()));
                }
            }
            drop(dual_node_internal);  // unlock
            // propagating nodes may be duplicated, but it's easy to check by `propagated_dual_node`
            for (vertex_ptr, grandson_dual_node) in propagating_vertices.into_iter() {
                let mut vertex = vertex_ptr.write(active_timestamp);
                if vertex.propagated_dual_node.is_none() {
                    vertex.propagated_dual_node = Some(dual_node_internal_ptr.downgrade());
                    vertex.propagated_grandson_dual_node = grandson_dual_node.clone();
                    // add to the sync list
                    if let Some(mirror_unit_weak) = &vertex.mirror_unit {
                        let unit_module_info = self.unit_module_info.as_mut().unwrap();
                        unit_module_info.sync_requests.push(SyncRequest {
                            mirror_unit_weak: mirror_unit_weak.clone(),
                            vertex_index: vertex.vertex_index,
                            propagated_dual_node: vertex.propagated_dual_node.clone().map(|weak| weak.upgrade_force().read_recursive().origin.clone()),
                            propagated_grandson_dual_node: vertex.propagated_grandson_dual_node.as_ref().map(|weak| weak.upgrade_force().read_recursive().origin.clone()),
                        });
                    }
                    let mut count_newly_propagated_edge = 0;
                    for edge_weak in vertex.edges.iter() {
                        let edge_ptr = edge_weak.upgrade_force();
                        let (is_left, newly_propagated_edge) = {
                            edge_ptr.dynamic_clear(active_timestamp);
                            let edge = edge_ptr.read_recursive(active_timestamp);
                            let is_left = vertex_ptr.downgrade() == edge.left;
                            let newly_propagated_edge = if is_left {
                                edge.left_dual_node.is_none()
                            } else {
                                edge.right_dual_node.is_none()
                            };
                            (is_left, newly_propagated_edge)
                        };
                        if newly_propagated_edge {
                            count_newly_propagated_edge += 1;
                            updated_boundary.push((is_left, edge_weak.clone()));
                            let mut edge = edge_ptr.write(active_timestamp);
                            if edge.weight == 0 {
                                newly_propagated_edge_has_zero_weight = true;
                            }
                            if is_left {
                                edge.left_dual_node = Some(dual_node_internal_ptr.downgrade());
                                edge.left_grandson_dual_node = grandson_dual_node.clone();
                            } else {
                                edge.right_dual_node = Some(dual_node_internal_ptr.downgrade());
                                edge.right_grandson_dual_node = grandson_dual_node.clone();
                            };
                        }
                    }
                    if count_newly_propagated_edge == 0 {
                        let mut dual_node_internal = dual_node_internal_ptr.write();
                        dual_node_internal.overgrown_stack.push((vertex_ptr.downgrade(), 0));
                    }
                }
            }
        } else {  // gracefully update the boundary to ease shrinking
            {
                let mut dual_node_internal = dual_node_internal_ptr.write();
                while dual_node_internal.overgrown_stack.len() > 0 {
                    let last_index = dual_node_internal.overgrown_stack.len() - 1;
                    let (_, overgrown) = &dual_node_internal.overgrown_stack[last_index];
                    if *overgrown == 0 {
                        let (vertex_weak, _) = dual_node_internal.overgrown_stack.pop().unwrap();
                        let vertex_ptr = vertex_weak.upgrade_force();
                        // push the surrounding edges back to the boundary
                        let mut vertex = vertex_ptr.write(active_timestamp);
                        if vertex.propagated_dual_node == Some(dual_node_internal_ptr.downgrade()) {
                            vertex.propagated_dual_node = None;
                            vertex.propagated_grandson_dual_node = None;
                            for edge_weak in vertex.edges.iter() {
                                let edge_ptr = edge_weak.upgrade_force();
                                let mut edge = edge_ptr.write(active_timestamp);
                                let is_left = vertex_ptr.downgrade() == edge.left;
                                if self.unit_module_info.is_none() {
                                    debug_assert!(if is_left {
                                        edge.left_dual_node == Some(dual_node_internal_ptr.downgrade()) && edge.left_grandson_dual_node.is_some()
                                    } else {
                                        edge.right_dual_node == Some(dual_node_internal_ptr.downgrade()) && edge.right_grandson_dual_node.is_some()
                                    }, "overgrown vertices must be surrounded by the same propagated dual node");
                                }
                                if is_left {
                                    edge.left_dual_node = None;
                                    edge.left_grandson_dual_node = None;
                                } else {
                                    edge.right_dual_node = None;
                                    edge.right_grandson_dual_node = None;
                                }
                                updated_boundary.push((!is_left, edge_weak.clone()));  // boundary has the opposite end
                            }
                        } else {
                            // this happens when sync request already vacate the vertex, thus no need to add edges to the boundary
                            // in fact, this will cause duplicate boundary edges if not skipped, leading to exceptions when creating blossoms
                            assert!(self.unit_module_info.is_some(), "serial module itself cannot happen");
                        }
                    } else {
                        break  // found non-negative overgrown in the stack
                    }
                }
            }
            let dual_node_internal = dual_node_internal_ptr.read_recursive();
            for (is_left, edge_weak) in dual_node_internal.boundary.iter() {
                let edge_ptr = edge_weak.upgrade_force();
                let is_left = *is_left;
                let edge = edge_ptr.read_recursive(active_timestamp);
                let this_growth = if is_left {
                    edge.left_growth
                } else {
                    edge.right_growth
                };
                if this_growth == 0 {
                    // need to shrink before this vertex
                    let this_vertex_ptr = if is_left {
                        edge.left.upgrade_force()
                    } else {
                        edge.right.upgrade_force()
                    };
                    // to avoid already occupied node being propagated
                    let this_vertex = this_vertex_ptr.read_recursive(active_timestamp);
                    if this_vertex.is_syndrome {  // never shrink from the syndrome itself
                        updated_boundary.push((is_left, edge_weak.clone()));
                    } else {
                        if edge.weight > 0 && self.unit_module_info.is_none() {  // do not check for 0-weight edges
                            assert!(this_vertex.propagated_dual_node.is_some(), "unexpected shrink into an empty vertex");
                        }
                        propagating_vertices.push((this_vertex_ptr.clone(), None));
                    }
                } else {  // keep other edges
                    updated_boundary.push((is_left, edge_weak.clone()));
                }
            }
            // propagating nodes may be duplicated, but it's easy to check by `propagated_dual_node`
            for (vertex_ptr, _) in propagating_vertices.into_iter() {
                let mut vertex = vertex_ptr.write(active_timestamp);
                if vertex.propagated_dual_node.is_some() {
                    vertex.propagated_dual_node = None;
                    vertex.propagated_grandson_dual_node = None;
                    for edge_weak in vertex.edges.iter() {
                        let edge_ptr = edge_weak.upgrade_force();
                        let (is_left, newly_propagated_edge) = {
                            let edge = edge_ptr.read_recursive(active_timestamp);
                            let is_left = vertex_ptr.downgrade() == edge.left;
                            debug_assert!(if is_left { edge.right != vertex_ptr.downgrade() } else { edge.right == vertex_ptr.downgrade() });
                            // fully grown edge is where to shrink
                            let newly_propagated_edge = edge.left_dual_node == Some(dual_node_internal_ptr.downgrade())
                                && edge.right_dual_node == Some(dual_node_internal_ptr.downgrade())
                                && edge.left_growth + edge.right_growth >= edge.weight;
                            debug_assert!({
                                newly_propagated_edge || {
                                    if is_left { edge.left_growth == 0  } else { edge.right_growth == 0 }
                                }
                            }, "an edge must be either newly propagated or to be removed");
                            (is_left, newly_propagated_edge)
                        };
                        if newly_propagated_edge {
                            updated_boundary.push((!is_left, edge_weak.clone()));
                            let mut edge = edge_ptr.write(active_timestamp);
                            if edge.weight == 0 {
                                newly_propagated_edge_has_zero_weight = true;
                            }
                            if is_left {
                                assert!(edge.right_dual_node.is_some(), "unexpected shrinking to empty edge");
                                assert!(edge.right_dual_node.as_ref().unwrap() == &dual_node_internal_ptr.downgrade(), "shrinking edge should be same tree node");
                                edge.left_dual_node = None;
                                edge.left_grandson_dual_node = None;
                            } else {
                                assert!(edge.left_dual_node.is_some(), "unexpected shrinking to empty edge");
                                assert!(edge.left_dual_node.as_ref().unwrap() == &dual_node_internal_ptr.downgrade(), "shrinking edge should be same tree node");
                                edge.right_dual_node = None;
                                edge.right_grandson_dual_node = None;
                            };
                        } else {
                            let mut edge = edge_ptr.write(active_timestamp);
                            if is_left {
                                edge.left_dual_node = None;
                                edge.left_grandson_dual_node = None;
                            } else {
                                edge.right_dual_node = None;
                                edge.right_grandson_dual_node = None;
                            }
                        }
                    }
                }
            }
        }
        // update the boundary
        let mut dual_node_internal = dual_node_internal_ptr.write();
        std::mem::swap(&mut updated_boundary, &mut dual_node_internal.boundary);
        // println!("{} boundary: {:?}", tree_node.boundary.len(), tree_node.boundary);
        if self.unit_module_info.is_none() {
            assert!(dual_node_internal.boundary.len() > 0, "the boundary of a dual cluster is never empty");
        }
        newly_propagated_edge_has_zero_weight
    }

    /// adjust the boundary of each dual node to fit into the need of growing (`length` > 0) or shrinking (`length` < 0)
    pub fn prepare_dual_node_growth(&mut self, dual_node_ptr: &DualNodePtr, is_grow: bool) {
        let mut need_another = self.prepare_dual_node_growth_single(dual_node_ptr, is_grow);
        while need_another {  // when there are 0 weight edges, one may need to run multiple iterations to get it prepared in a proper state
            need_another = self.prepare_dual_node_growth_single(dual_node_ptr, is_grow);
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::example::*;
    use super::super::primal_module_serial::tests::*;

    #[allow(dead_code)]
    fn debug_print_dual_node(dual_module: &DualModuleSerial, dual_node_ptr: &DualNodePtr) {
        println!("boundary:");
        let dual_node_internal_ptr = dual_module.get_dual_node_internal_ptr(dual_node_ptr);
        let dual_node_internal = dual_node_internal_ptr.read_recursive();
        for (is_left, edge_weak) in dual_node_internal.boundary.iter() {
            let edge_ptr = edge_weak.upgrade_force();
            let edge = edge_ptr.read_recursive_force();
            println!("    {} {:?}", if *is_left { " left" } else { "right" }, edge);
        }
    }

    #[test]
    fn dual_module_serial_basics() {  // cargo test dual_module_serial_basics -- --nocapture
        let visualize_filename = format!("dual_module_serial_basics.json");
        let half_weight = 500;
        let mut code = CodeCapacityPlanarCode::new(7, 0.1, half_weight);
        let mut visualizer = Visualizer::new(Some(visualize_data_folder() + visualize_filename.as_str())).unwrap();
        visualizer.set_positions(code.get_positions(), true);  // automatic center all nodes
        print_visualize_link(&visualize_filename);
        // create dual module
        let initializer = code.get_initializer();
        let mut dual_module = DualModuleSerial::new(&initializer);
        // try to work on a simple syndrome
        code.vertices[19].is_syndrome = true;
        code.vertices[25].is_syndrome = true;
        let interface = DualModuleInterface::new(&code.get_syndrome(), &mut dual_module);
        visualizer.snapshot_combined(format!("syndrome"), vec![&interface, &dual_module]).unwrap();
        // create dual nodes and grow them by half length
        let dual_node_19_ptr = interface.nodes[0].as_ref().unwrap().clone();
        let dual_node_25_ptr = interface.nodes[1].as_ref().unwrap().clone();
        dual_module.grow_dual_node(&dual_node_19_ptr, half_weight);
        dual_module.grow_dual_node(&dual_node_25_ptr, half_weight);
        visualizer.snapshot_combined(format!("grow to 0.5"), vec![&interface, &dual_module]).unwrap();
        dual_module.grow_dual_node(&dual_node_19_ptr, half_weight);
        dual_module.grow_dual_node(&dual_node_25_ptr, half_weight);
        visualizer.snapshot_combined(format!("grow to 1"), vec![&interface, &dual_module]).unwrap();
        dual_module.grow_dual_node(&dual_node_19_ptr, half_weight);
        dual_module.grow_dual_node(&dual_node_25_ptr, half_weight);
        visualizer.snapshot_combined(format!("grow to 1.5"), vec![&interface, &dual_module]).unwrap();
        dual_module.grow_dual_node(&dual_node_19_ptr, -half_weight);
        dual_module.grow_dual_node(&dual_node_25_ptr, -half_weight);
        visualizer.snapshot_combined(format!("shrink to 1"), vec![&interface, &dual_module]).unwrap();
        dual_module.grow_dual_node(&dual_node_19_ptr, -half_weight);
        dual_module.grow_dual_node(&dual_node_25_ptr, -half_weight);
        visualizer.snapshot_combined(format!("shrink to 0.5"), vec![&interface, &dual_module]).unwrap();
        dual_module.grow_dual_node(&dual_node_19_ptr, -half_weight);
        dual_module.grow_dual_node(&dual_node_25_ptr, -half_weight);
        visualizer.snapshot_combined(format!("shrink to 0"), vec![&interface, &dual_module]).unwrap();
    }

    #[test]
    fn dual_module_serial_blossom_basics() {  // cargo test dual_module_serial_blossom_basics -- --nocapture
        let visualize_filename = format!("dual_module_serial_blossom_basics.json");
        let half_weight = 500;
        let mut code = CodeCapacityPlanarCode::new(7, 0.1, half_weight);
        let mut visualizer = Visualizer::new(Some(visualize_data_folder() + visualize_filename.as_str())).unwrap();
        visualizer.set_positions(code.get_positions(), true);  // automatic center all nodes
        print_visualize_link(&visualize_filename);
        // create dual module
        let initializer = code.get_initializer();
        let mut dual_module = DualModuleSerial::new(&initializer);
        // try to work on a simple syndrome
        code.vertices[19].is_syndrome = true;
        code.vertices[26].is_syndrome = true;
        code.vertices[35].is_syndrome = true;
        let mut interface = DualModuleInterface::new(&code.get_syndrome(), &mut dual_module);
        visualizer.snapshot_combined(format!("syndrome"), vec![&interface, &dual_module]).unwrap();
        // create dual nodes and grow them by half length
        let dual_node_19_ptr = interface.nodes[0].as_ref().unwrap().clone();
        let dual_node_26_ptr = interface.nodes[1].as_ref().unwrap().clone();
        let dual_node_35_ptr = interface.nodes[2].as_ref().unwrap().clone();
        interface.grow(2 * half_weight, &mut dual_module);
        assert_eq!(interface.sum_dual_variables, 6 * half_weight);
        visualizer.snapshot_combined(format!("before create blossom"), vec![&interface, &dual_module]).unwrap();
        let nodes_circle = vec![dual_node_19_ptr.clone(), dual_node_26_ptr.clone(), dual_node_35_ptr.clone()];
        interface.set_grow_state(&dual_node_26_ptr, DualNodeGrowState::Shrink, &mut dual_module);
        let dual_node_blossom = interface.create_blossom(nodes_circle, vec![], &mut dual_module);
        interface.grow(half_weight, &mut dual_module);
        assert_eq!(interface.sum_dual_variables, 7 * half_weight);
        visualizer.snapshot_combined(format!("blossom grow half weight"), vec![&interface, &dual_module]).unwrap();
        interface.grow(half_weight, &mut dual_module);
        assert_eq!(interface.sum_dual_variables, 8 * half_weight);
        visualizer.snapshot_combined(format!("blossom grow half weight"), vec![&interface, &dual_module]).unwrap();
        interface.grow(half_weight, &mut dual_module);
        assert_eq!(interface.sum_dual_variables, 9 * half_weight);
        visualizer.snapshot_combined(format!("blossom grow half weight"), vec![&interface, &dual_module]).unwrap();
        interface.set_grow_state(&dual_node_blossom, DualNodeGrowState::Shrink, &mut dual_module);
        interface.grow(half_weight, &mut dual_module);
        assert_eq!(interface.sum_dual_variables, 8 * half_weight);
        visualizer.snapshot_combined(format!("blossom shrink half weight"), vec![&interface, &dual_module]).unwrap();
        interface.grow(2 * half_weight, &mut dual_module);
        assert_eq!(interface.sum_dual_variables, 6 * half_weight);
        visualizer.snapshot_combined(format!("blossom shrink weight"), vec![&interface, &dual_module]).unwrap();
        interface.expand_blossom(dual_node_blossom, &mut dual_module);
        interface.set_grow_state(&dual_node_19_ptr, DualNodeGrowState::Shrink, &mut dual_module);
        interface.set_grow_state(&dual_node_26_ptr, DualNodeGrowState::Shrink, &mut dual_module);
        interface.set_grow_state(&dual_node_35_ptr, DualNodeGrowState::Shrink, &mut dual_module);
        interface.grow(half_weight, &mut dual_module);
        assert_eq!(interface.sum_dual_variables, 3 * half_weight);
        visualizer.snapshot_combined(format!("individual shrink half weight"), vec![&interface, &dual_module]).unwrap();
    }

    #[test]
    fn dual_module_serial_stop_reason_1() {  // cargo test dual_module_serial_stop_reason_1 -- --nocapture
        let visualize_filename = format!("dual_module_serial_stop_reason_1.json");
        let half_weight = 500;
        let mut code = CodeCapacityPlanarCode::new(7, 0.1, half_weight);
        let mut visualizer = Visualizer::new(Some(visualize_data_folder() + visualize_filename.as_str())).unwrap();
        visualizer.set_positions(code.get_positions(), true);  // automatic center all nodes
        print_visualize_link(&visualize_filename);
        // create dual module
        let initializer = code.get_initializer();
        let mut dual_module = DualModuleSerial::new(&initializer);
        // try to work on a simple syndrome
        code.vertices[19].is_syndrome = true;
        code.vertices[25].is_syndrome = true;
        let mut interface = DualModuleInterface::new(&code.get_syndrome(), &mut dual_module);
        visualizer.snapshot_combined(format!("syndrome"), vec![&interface, &dual_module]).unwrap();
        // create dual nodes and grow them by half length
        let dual_node_19_ptr = interface.nodes[0].as_ref().unwrap().clone();
        let dual_node_25_ptr = interface.nodes[1].as_ref().unwrap().clone();
        // grow the maximum
        let group_max_update_length = dual_module.compute_maximum_update_length();
        assert_eq!(group_max_update_length.get_none_zero_growth(), Some(2 * half_weight), "unexpected: {:?}", group_max_update_length);
        interface.grow(2 * half_weight, &mut dual_module);
        assert_eq!(interface.sum_dual_variables, 4 * half_weight);
        visualizer.snapshot_combined(format!("grow"), vec![&interface, &dual_module]).unwrap();
        // grow the maximum
        let group_max_update_length = dual_module.compute_maximum_update_length();
        assert_eq!(group_max_update_length.get_none_zero_growth(), Some(half_weight), "unexpected: {:?}", group_max_update_length);
        interface.grow(half_weight, &mut dual_module);
        assert_eq!(interface.sum_dual_variables, 6 * half_weight);
        visualizer.snapshot_combined(format!("grow"), vec![&interface, &dual_module]).unwrap();
        // cannot grow anymore, find out the reason
        let group_max_update_length = dual_module.compute_maximum_update_length();
        assert!(group_max_update_length.get_conflicts_immutable().peek().unwrap().is_conflicting(&dual_node_19_ptr, &dual_node_25_ptr), "unexpected: {:?}", group_max_update_length);
    }

    #[test]
    fn dual_module_serial_stop_reason_2() {  // cargo test dual_module_serial_stop_reason_2 -- --nocapture
        let visualize_filename = format!("dual_module_serial_stop_reason_2.json");
        let half_weight = 500;
        let mut code = CodeCapacityPlanarCode::new(7, 0.1, half_weight);
        let mut visualizer = Visualizer::new(Some(visualize_data_folder() + visualize_filename.as_str())).unwrap();
        visualizer.set_positions(code.get_positions(), true);  // automatic center all nodes
        print_visualize_link(&visualize_filename);
        // create dual module
        let initializer = code.get_initializer();
        let mut dual_module = DualModuleSerial::new(&initializer);
        // try to work on a simple syndrome
        code.vertices[18].is_syndrome = true;
        code.vertices[26].is_syndrome = true;
        code.vertices[34].is_syndrome = true;
        let mut interface = DualModuleInterface::new(&code.get_syndrome(), &mut dual_module);
        visualizer.snapshot_combined(format!("syndrome"), vec![&interface, &dual_module]).unwrap();
        // create dual nodes and grow them by half length
        let dual_node_18_ptr = interface.nodes[0].as_ref().unwrap().clone();
        let dual_node_26_ptr = interface.nodes[1].as_ref().unwrap().clone();
        let dual_node_34_ptr = interface.nodes[2].as_ref().unwrap().clone();
        // grow the maximum
        let group_max_update_length = dual_module.compute_maximum_update_length();
        assert_eq!(group_max_update_length.get_none_zero_growth(), Some(half_weight), "unexpected: {:?}", group_max_update_length);
        interface.grow(half_weight, &mut dual_module);
        assert_eq!(interface.sum_dual_variables, 3 * half_weight);
        visualizer.snapshot_combined(format!("grow"), vec![&interface, &dual_module]).unwrap();
        // cannot grow anymore, find out the reason
        let group_max_update_length = dual_module.compute_maximum_update_length();
        assert!(group_max_update_length.get_conflicts_immutable().peek().unwrap().is_conflicting(&dual_node_18_ptr, &dual_node_26_ptr)
            || group_max_update_length.get_conflicts_immutable().peek().unwrap().is_conflicting(&dual_node_26_ptr, &dual_node_34_ptr), "unexpected: {:?}", group_max_update_length);
        // first match 18 and 26
        interface.set_grow_state(&dual_node_18_ptr, DualNodeGrowState::Stay, &mut dual_module);
        interface.set_grow_state(&dual_node_26_ptr, DualNodeGrowState::Stay, &mut dual_module);
        // cannot grow anymore, find out the reason
        let group_max_update_length = dual_module.compute_maximum_update_length();
        assert!(group_max_update_length.get_conflicts_immutable().peek().unwrap().is_conflicting(&dual_node_26_ptr, &dual_node_34_ptr)
            , "unexpected: {:?}", group_max_update_length);
        // 34 touches 26, so it will grow the tree by absorbing 18 and 26
        interface.set_grow_state(&dual_node_18_ptr, DualNodeGrowState::Grow, &mut dual_module);
        interface.set_grow_state(&dual_node_26_ptr, DualNodeGrowState::Shrink, &mut dual_module);
        // grow the maximum
        let group_max_update_length = dual_module.compute_maximum_update_length();
        assert_eq!(group_max_update_length.get_none_zero_growth(), Some(half_weight), "unexpected: {:?}", group_max_update_length);
        interface.grow(half_weight, &mut dual_module);
        assert_eq!(interface.sum_dual_variables, 4 * half_weight);
        visualizer.snapshot_combined(format!("grow"), vec![&interface, &dual_module]).unwrap();
        // cannot grow anymore, find out the reason
        let group_max_update_length = dual_module.compute_maximum_update_length();
        assert!(group_max_update_length.get_conflicts_immutable().peek().unwrap().is_conflicting(&dual_node_18_ptr, &dual_node_34_ptr), "unexpected: {:?}", group_max_update_length);
        // for a blossom because 18 and 34 come from the same alternating tree
        let dual_node_blossom = interface.create_blossom(vec![dual_node_18_ptr.clone(), dual_node_26_ptr.clone(), dual_node_34_ptr.clone()], vec![], &mut dual_module);
        // grow the maximum
        let group_max_update_length = dual_module.compute_maximum_update_length();
        assert_eq!(group_max_update_length.get_none_zero_growth(), Some(2 * half_weight), "unexpected: {:?}", group_max_update_length);
        interface.grow(2 * half_weight, &mut dual_module);
        assert_eq!(interface.sum_dual_variables, 6 * half_weight);
        visualizer.snapshot_combined(format!("grow blossom"), vec![&interface, &dual_module]).unwrap();
        // grow the maximum
        let group_max_update_length = dual_module.compute_maximum_update_length();
        assert_eq!(group_max_update_length.get_none_zero_growth(), Some(2 * half_weight), "unexpected: {:?}", group_max_update_length);
        interface.grow(2 * half_weight, &mut dual_module);
        assert_eq!(interface.sum_dual_variables, 8 * half_weight);
        visualizer.snapshot_combined(format!("grow blossom"), vec![&interface, &dual_module]).unwrap();
        // cannot grow anymore, find out the reason
        let group_max_update_length = dual_module.compute_maximum_update_length();
        assert!(group_max_update_length.get_conflicts_immutable().peek().unwrap().get_touching_virtual() == Some((dual_node_blossom.clone(), 23))
            || group_max_update_length.get_conflicts_immutable().peek().unwrap().get_touching_virtual() == Some((dual_node_blossom.clone(), 39))
            , "unexpected: {:?}", group_max_update_length);
        // blossom touches virtual boundary, so it's matched
        interface.set_grow_state(&dual_node_blossom, DualNodeGrowState::Stay, &mut dual_module);
        let group_max_update_length = dual_module.compute_maximum_update_length();
        assert!(group_max_update_length.is_empty(), "unexpected: {:?}", group_max_update_length);
        // also test the reverse procedure: shrinking and expanding blossom
        interface.set_grow_state(&dual_node_blossom, DualNodeGrowState::Shrink, &mut dual_module);
        let group_max_update_length = dual_module.compute_maximum_update_length();
        assert_eq!(group_max_update_length.get_none_zero_growth(), Some(2 * half_weight), "unexpected: {:?}", group_max_update_length);
        interface.grow(2 * half_weight, &mut dual_module);
        assert_eq!(interface.sum_dual_variables, 6 * half_weight);
        visualizer.snapshot_combined(format!("shrink blossom"), vec![&interface, &dual_module]).unwrap();
        // before expand
        interface.set_grow_state(&dual_node_blossom, DualNodeGrowState::Shrink, &mut dual_module);
        let group_max_update_length = dual_module.compute_maximum_update_length();
        assert_eq!(group_max_update_length.get_none_zero_growth(), Some(2 * half_weight), "unexpected: {:?}", group_max_update_length);
        interface.grow(2 * half_weight, &mut dual_module);
        assert_eq!(interface.sum_dual_variables, 4 * half_weight);
        visualizer.snapshot_combined(format!("shrink blossom"), vec![&interface, &dual_module]).unwrap();
        // cannot shrink anymore, find out the reason
        let group_max_update_length = dual_module.compute_maximum_update_length();
        assert!(group_max_update_length.get_conflicts_immutable().peek().unwrap() == &MaxUpdateLength::BlossomNeedExpand(dual_node_blossom.downgrade())
            , "unexpected: {:?}", group_max_update_length);
        // expand blossom
        interface.expand_blossom(dual_node_blossom, &mut dual_module);
        // regain access to underlying nodes
        interface.set_grow_state(&dual_node_18_ptr, DualNodeGrowState::Shrink, &mut dual_module);
        interface.set_grow_state(&dual_node_26_ptr, DualNodeGrowState::Grow, &mut dual_module);
        interface.set_grow_state(&dual_node_34_ptr, DualNodeGrowState::Shrink, &mut dual_module);
        let group_max_update_length = dual_module.compute_maximum_update_length();
        assert_eq!(group_max_update_length.get_none_zero_growth(), Some(2 * half_weight), "unexpected: {:?}", group_max_update_length);
        interface.grow(2 * half_weight, &mut dual_module);
        assert_eq!(interface.sum_dual_variables, 2 * half_weight);
        visualizer.snapshot_combined(format!("shrink"), vec![&interface, &dual_module]).unwrap();
    }

    /// this test helps observe bugs of fast clear, by removing snapshot: snapshot will do the clear automatically
    #[test]
    fn dual_module_serial_fast_clear_1() {  // cargo test dual_module_serial_fast_clear_1 -- --nocapture
        let half_weight = 500;
        let mut code = CodeCapacityPlanarCode::new(7, 0.1, half_weight);
        // create dual module
        let initializer = code.get_initializer();
        let mut dual_module = DualModuleSerial::new(&initializer);
        // try to work on a simple syndrome
        code.vertices[18].is_syndrome = true;
        code.vertices[26].is_syndrome = true;
        code.vertices[34].is_syndrome = true;
        let mut interface = DualModuleInterface::new(&code.get_syndrome(), &mut dual_module);
        // create dual nodes and grow them by half length
        let dual_node_18_ptr = interface.nodes[0].as_ref().unwrap().clone();
        let dual_node_26_ptr = interface.nodes[1].as_ref().unwrap().clone();
        let dual_node_34_ptr = interface.nodes[2].as_ref().unwrap().clone();
        // grow the maximum
        let group_max_update_length = dual_module.compute_maximum_update_length();
        assert_eq!(group_max_update_length.get_none_zero_growth(), Some(half_weight), "unexpected: {:?}", group_max_update_length);
        interface.grow(half_weight, &mut dual_module);
        assert_eq!(interface.sum_dual_variables, 3 * half_weight);
        // cannot grow anymore, find out the reason
        let group_max_update_length = dual_module.compute_maximum_update_length();
        assert!(group_max_update_length.get_conflicts_immutable().peek().unwrap().is_conflicting(&dual_node_18_ptr, &dual_node_26_ptr)
            || group_max_update_length.get_conflicts_immutable().peek().unwrap().is_conflicting(&dual_node_26_ptr, &dual_node_34_ptr), "unexpected: {:?}", group_max_update_length);
        // first match 18 and 26
        interface.set_grow_state(&dual_node_18_ptr, DualNodeGrowState::Stay, &mut dual_module);
        interface.set_grow_state(&dual_node_26_ptr, DualNodeGrowState::Stay, &mut dual_module);
        // cannot grow anymore, find out the reason
        let group_max_update_length = dual_module.compute_maximum_update_length();
        assert!(group_max_update_length.get_conflicts_immutable().peek().unwrap().is_conflicting(&dual_node_26_ptr, &dual_node_34_ptr)
            , "unexpected: {:?}", group_max_update_length);
        // 34 touches 26, so it will grow the tree by absorbing 18 and 26
        interface.set_grow_state(&dual_node_18_ptr, DualNodeGrowState::Grow, &mut dual_module);
        interface.set_grow_state(&dual_node_26_ptr, DualNodeGrowState::Shrink, &mut dual_module);
        // grow the maximum
        let group_max_update_length = dual_module.compute_maximum_update_length();
        assert_eq!(group_max_update_length.get_none_zero_growth(), Some(half_weight), "unexpected: {:?}", group_max_update_length);
        interface.grow(half_weight, &mut dual_module);
        assert_eq!(interface.sum_dual_variables, 4 * half_weight);
        // cannot grow anymore, find out the reason
        let group_max_update_length = dual_module.compute_maximum_update_length();
        assert!(group_max_update_length.get_conflicts_immutable().peek().unwrap().is_conflicting(&dual_node_18_ptr, &dual_node_34_ptr), "unexpected: {:?}", group_max_update_length);
        // for a blossom because 18 and 34 come from the same alternating tree
        let dual_node_blossom = interface.create_blossom(vec![dual_node_18_ptr.clone(), dual_node_26_ptr.clone(), dual_node_34_ptr.clone()], vec![], &mut dual_module);
        // grow the maximum
        let group_max_update_length = dual_module.compute_maximum_update_length();
        assert_eq!(group_max_update_length.get_none_zero_growth(), Some(2 * half_weight), "unexpected: {:?}", group_max_update_length);
        interface.grow(2 * half_weight, &mut dual_module);
        // grow the maximum
        let group_max_update_length = dual_module.compute_maximum_update_length();
        assert_eq!(group_max_update_length.get_none_zero_growth(), Some(2 * half_weight), "unexpected: {:?}", group_max_update_length);
        interface.grow(2 * half_weight, &mut dual_module);
        // cannot grow anymore, find out the reason
        let group_max_update_length = dual_module.compute_maximum_update_length();
        assert!(group_max_update_length.get_conflicts_immutable().peek().unwrap().get_touching_virtual() == Some((dual_node_blossom.clone(), 23))
            || group_max_update_length.get_conflicts_immutable().peek().unwrap().get_touching_virtual() == Some((dual_node_blossom.clone(), 39))
            , "unexpected: {:?}", group_max_update_length);
        // blossom touches virtual boundary, so it's matched
        interface.set_grow_state(&dual_node_blossom, DualNodeGrowState::Stay, &mut dual_module);
        let group_max_update_length = dual_module.compute_maximum_update_length();
        assert!(group_max_update_length.is_empty(), "unexpected: {:?}", group_max_update_length);
        // also test the reverse procedure: shrinking and expanding blossom
        interface.set_grow_state(&dual_node_blossom, DualNodeGrowState::Shrink, &mut dual_module);
        let group_max_update_length = dual_module.compute_maximum_update_length();
        assert_eq!(group_max_update_length.get_none_zero_growth(), Some(2 * half_weight), "unexpected: {:?}", group_max_update_length);
        interface.grow(2 * half_weight, &mut dual_module);
        // before expand
        interface.set_grow_state(&dual_node_blossom, DualNodeGrowState::Shrink, &mut dual_module);
        let group_max_update_length = dual_module.compute_maximum_update_length();
        assert_eq!(group_max_update_length.get_none_zero_growth(), Some(2 * half_weight), "unexpected: {:?}", group_max_update_length);
        interface.grow(2 * half_weight, &mut dual_module);
        // cannot shrink anymore, find out the reason
        let group_max_update_length = dual_module.compute_maximum_update_length();
        assert!(group_max_update_length.get_conflicts_immutable().peek().unwrap() == &MaxUpdateLength::BlossomNeedExpand(dual_node_blossom.downgrade())
            , "unexpected: {:?}", group_max_update_length);
        // expand blossom
        interface.expand_blossom(dual_node_blossom, &mut dual_module);
        // regain access to underlying nodes
        interface.set_grow_state(&dual_node_18_ptr, DualNodeGrowState::Shrink, &mut dual_module);
        interface.set_grow_state(&dual_node_26_ptr, DualNodeGrowState::Grow, &mut dual_module);
        interface.set_grow_state(&dual_node_34_ptr, DualNodeGrowState::Shrink, &mut dual_module);
        let group_max_update_length = dual_module.compute_maximum_update_length();
        assert_eq!(group_max_update_length.get_none_zero_growth(), Some(2 * half_weight), "unexpected: {:?}", group_max_update_length);
        interface.grow(2 * half_weight, &mut dual_module);
        assert_eq!(interface.sum_dual_variables, 2 * half_weight);
    }

    #[test]
    fn dual_module_grow_iterative_1() {  // cargo test dual_module_grow_iterative_1 -- --nocapture
        let visualize_filename = format!("dual_module_grow_iterative_1.json");
        let half_weight = 500;
        let mut code = CodeCapacityPlanarCode::new(11, 0.1, half_weight);
        let mut visualizer = Visualizer::new(Some(visualize_data_folder() + visualize_filename.as_str())).unwrap();
        visualizer.set_positions(code.get_positions(), true);  // automatic center all nodes
        print_visualize_link(&visualize_filename);
        // create dual module
        let initializer = code.get_initializer();
        let mut dual_module = DualModuleSerial::new(&initializer);
        // try to work on a simple syndrome
        code.vertices[39].is_syndrome = true;
        code.vertices[65].is_syndrome = true;
        code.vertices[87].is_syndrome = true;
        let mut interface = DualModuleInterface::new(&code.get_syndrome(), &mut dual_module);
        visualizer.snapshot_combined(format!("syndrome"), vec![&interface, &dual_module]).unwrap();
        // create dual nodes and grow them by half length
        interface.grow_iterative(4 * half_weight, &mut dual_module);
        visualizer.snapshot_combined(format!("grow"), vec![&interface, &dual_module]).unwrap();
        assert_eq!(interface.sum_dual_variables, 3 * 4 * half_weight);
        let dual_node_39_ptr = interface.nodes[0].as_ref().unwrap().clone();
        let dual_node_65_ptr = interface.nodes[1].as_ref().unwrap().clone();
        let dual_node_87_ptr = interface.nodes[2].as_ref().unwrap().clone();
        interface.set_grow_state(&dual_node_39_ptr, DualNodeGrowState::Shrink, &mut dual_module);
        interface.set_grow_state(&dual_node_65_ptr, DualNodeGrowState::Shrink, &mut dual_module);
        interface.set_grow_state(&dual_node_87_ptr, DualNodeGrowState::Shrink, &mut dual_module);
        interface.grow_iterative(4 * half_weight, &mut dual_module);
        visualizer.snapshot_combined(format!("shrink"), vec![&interface, &dual_module]).unwrap();
        assert_eq!(interface.sum_dual_variables, 0);
    }

    #[test]
    fn dual_module_debug_1() {  // cargo test dual_module_debug_1 -- --nocapture
        let visualize_filename = format!("dual_module_debug_1.json");
        let syndrome_vertices = vec![6, 7, 17, 18, 21, 27, 28, 42, 43, 49, 51, 52, 54, 55, 61, 63, 65, 67, 76, 78, 80, 86, 103, 110, 113, 114, 116, 122, 125, 127];
        primal_module_serial_basic_standard_syndrome(11, visualize_filename, syndrome_vertices, 23);
    }

    #[test]
    fn dual_module_debug_2() {  // cargo test dual_module_debug_2 -- --nocapture
        let visualize_filename = format!("dual_module_debug_2.json");
        let syndrome_vertices = vec![5, 12, 16, 19, 21, 38, 42, 43, 49, 56, 61, 67, 72, 73, 74, 75, 76, 88, 89, 92, 93, 99, 105, 112, 117, 120, 124, 129];
        primal_module_serial_basic_standard_syndrome(11, visualize_filename, syndrome_vertices, 22);
    }

    #[test]
    fn dual_module_erasure_1() {  // cargo test dual_module_erasure_1 -- --nocapture
        let visualize_filename = format!("dual_module_erasure_1.json");
        let half_weight = 500;
        let mut code = CodeCapacityPlanarCode::new(11, 0.1, half_weight);
        let mut visualizer = Visualizer::new(Some(visualize_data_folder() + visualize_filename.as_str())).unwrap();
        visualizer.set_positions(code.get_positions(), true);  // automatic center all nodes
        print_visualize_link(&visualize_filename);
        // create dual module
        let initializer = code.get_initializer();
        let mut dual_module = DualModuleSerial::new(&initializer);
        // try to work on a simple syndrome
        code.vertices[64].is_syndrome = true;
        let mut interface = DualModuleInterface::new(&code.get_syndrome(), &mut dual_module);
        visualizer.snapshot_combined(format!("syndrome"), vec![&interface, &dual_module]).unwrap();
        // generate erasures and load them to the dual module
        let erasures = vec![110, 78, 57, 142, 152, 163, 164];
        dual_module.load_erasures(&erasures);
        visualizer.snapshot_combined(format!("load erasure"), vec![&interface, &dual_module]).unwrap();
        // create dual nodes and grow them by half length
        for _ in 0..3 {
            interface.grow_iterative(2 * half_weight, &mut dual_module);
            visualizer.snapshot_combined(format!("grow"), vec![&interface, &dual_module]).unwrap();
        }
        // set them to shrink
        let dual_node_ptr = interface.nodes[0].as_ref().unwrap().clone();
        interface.set_grow_state(&dual_node_ptr, DualNodeGrowState::Shrink, &mut dual_module);
        // shrink them back, to make sure the operation is reversible
        for _ in 0..3 {
            interface.grow_iterative(2 * half_weight, &mut dual_module);
            visualizer.snapshot_combined(format!("shrink"), vec![&interface, &dual_module]).unwrap();
        }
        // cancel the erasures and grow the dual module in normal case, this should automatically clear the erasures
        let mut interface = DualModuleInterface::new(&code.get_syndrome(), &mut dual_module);
        visualizer.snapshot_combined(format!("after clear"), vec![&interface, &dual_module]).unwrap();
        dual_module.load_erasures(&vec![]);  // no erasures this time, to test if the module recovers correctly
        for _ in 0..3 {
            interface.grow_iterative(2 * half_weight, &mut dual_module);
            visualizer.snapshot_combined(format!("grow"), vec![&interface, &dual_module]).unwrap();
        }
    }
    
}
