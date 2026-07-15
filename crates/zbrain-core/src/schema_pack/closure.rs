//! Alias graph BFS closure (E8 refinement of D12).
//!
//! Closure is driven by an explicit alias graph. Each pack type declares
//! `aliases: [other-type, ...]`. The closure of type T is the BFS traversal
//! starting at T, following both A→B edges (per A's declaration) and B→A
//! edges (per B's declaration if present). This is "symmetric per
//! declaration." Transitive cap = 4.
//!
//! Ported from TS `src/core/schema-pack/closure.ts`.

use std::collections::{HashMap, HashSet, VecDeque};

use super::manifest::SchemaPackManifest;

/// Maximum BFS depth for alias closure resolution.
pub const ALIAS_CLOSURE_MAX_DEPTH: usize = 4;

/// Resolved alias graph keyed by type name. Each entry is the set of types
/// that share a symmetric alias edge with the keyed type (one hop).
pub type AliasGraph = HashMap<String, HashSet<String>>;

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

/// Thrown when a cycle is detected in the alias graph at build time.
#[derive(Debug, Clone)]
pub struct AliasCycleError {
    pub path: Vec<String>,
}

impl std::fmt::Display for AliasCycleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "alias cycle detected: {}", self.path.join(" → "))
    }
}

impl std::error::Error for AliasCycleError {}

/// Thrown when BFS closure exceeds ALIAS_CLOSURE_MAX_DEPTH.
#[derive(Debug, Clone)]
pub struct AliasDepthExceededError {
    pub query_type: String,
    pub depth: usize,
}

impl std::fmt::Display for AliasDepthExceededError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "alias closure for \"{}\" exceeded max depth {} at depth {}",
            self.query_type,
            ALIAS_CLOSURE_MAX_DEPTH,
            self.depth
        )
    }
}

impl std::error::Error for AliasDepthExceededError {}

// ---------------------------------------------------------------------------
// buildAliasGraph
// ---------------------------------------------------------------------------

/// Build the symmetric per-declaration alias graph from the manifest.
/// Returns `Err(AliasCycleError)` if a cycle is detected.
pub fn build_alias_graph(
    manifest: &SchemaPackManifest,
) -> Result<AliasGraph, AliasCycleError> {
    let mut adj: HashMap<String, HashSet<String>> = HashMap::new();

    for pt in &manifest.page_types {
        adj.entry(pt.name.clone()).or_default();
        for alias in &pt.aliases {
            // Symmetric per declaration: A declares [B] → both A→B and B→A.
            adj.entry(pt.name.clone())
                .or_default()
                .insert(alias.clone());
            adj.entry(alias.clone())
                .or_default()
                .insert(pt.name.clone());
        }
    }

    detect_cycles(&adj)?;
    Ok(adj)
}

/// DFS node color (white/gray/black) for cycle detection.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DfsColor {
    White,
    Gray,
    Black,
}

/// DFS cycle detection (white/gray/black). Returns `Err` on back-edge
/// that is not the immediate parent (symmetric mirror).
fn detect_cycles(adj: &HashMap<String, HashSet<String>>) -> Result<(), AliasCycleError> {
    let mut color: HashMap<String, DfsColor> = HashMap::new();
    for node in adj.keys() {
        color.insert(node.clone(), DfsColor::White);
    }

    // Iterative DFS to avoid nested closure lifetime issues.
    for node in adj.keys() {
        if color.get(node) == Some(&DfsColor::White) {
            dfs_detect(node, adj, &mut color)?;
        }
    }

    Ok(())
}

fn dfs_detect(
    start: &str,
    adj: &HashMap<String, HashSet<String>>,
    color: &mut HashMap<String, DfsColor>,
) -> Result<(), AliasCycleError> {
    // (node, parent, index_in_path) stack
    struct Frame {
        node: String,
        parent: Option<String>,
        neighbor_idx: usize,
    }

    let mut stack: Vec<Frame> = Vec::new();
    let mut path: Vec<String> = Vec::new();

    color.insert(start.to_string(), DfsColor::Gray);
    path.push(start.to_string());
    stack.push(Frame {
        node: start.to_string(),
        parent: None,
        neighbor_idx: 0,
    });

    while let Some(frame) = stack.last_mut() {
        let node = frame.node.clone();
        let idx = frame.neighbor_idx;
        let neighbors: Vec<&String> = adj
            .get(&node)
            .map(|s| s.iter().collect())
            .unwrap_or_default();

        if idx >= neighbors.len() {
            // Done with this node.
            color.insert(node.clone(), DfsColor::Black);
            stack.pop();
            path.pop();
            continue;
        }

        let next = neighbors[idx].clone();
        frame.neighbor_idx += 1;

        let c = color.get(&next).copied().unwrap_or(DfsColor::White);
        match c {
            DfsColor::Gray => {
                // Back-edge. Check if it's the immediate parent
                // (symmetric mirror, not a real cycle).
                let is_parent = frame.parent.as_deref() == Some(&next);
                if !is_parent {
                    let cycle_start = path.iter().position(|n| n == &next).unwrap();
                    let cycle_path: Vec<String> = path[cycle_start..]
                        .iter()
                        .cloned()
                        .chain(std::iter::once(next.clone()))
                        .collect();
                    return Err(AliasCycleError { path: cycle_path });
                }
            }
            DfsColor::White => {
                color.insert(next.clone(), DfsColor::Gray);
                path.push(next.clone());
                stack.push(Frame {
                    node: next.clone(),
                    parent: Some(node.clone()),
                    neighbor_idx: 0,
                });
            }
            DfsColor::Black => {
                // Already fully explored; skip.
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// expandClosure
// ---------------------------------------------------------------------------

/// Options for `expand_closure`.
pub struct ExpandClosureOpts {
    /// Called when depth cap is hit with a non-empty frontier.
    pub on_depth_exceeded: Option<Box<dyn FnMut(&str)>>,
}

impl Default for ExpandClosureOpts {
    fn default() -> Self {
        Self { on_depth_exceeded: None }
    }
}

/// BFS closure of a query type over the alias graph. Caps at
/// ALIAS_CLOSURE_MAX_DEPTH = 4. Returns the set of types that should be
/// included in a query for the input type, sorted lexicographically for
/// deterministic output (test snapshots + cache keys).
pub fn expand_closure(
    query_type: &str,
    graph: &AliasGraph,
    opts: &mut ExpandClosureOpts,
) -> Vec<String> {
    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(query_type.to_string());

    let mut frontier: VecDeque<String> = VecDeque::new();
    frontier.push_back(query_type.to_string());

    let mut depth = 0usize;

    while !frontier.is_empty() && depth < ALIAS_CLOSURE_MAX_DEPTH {
        let level_size = frontier.len();
        for _ in 0..level_size {
            let t = frontier.pop_front().unwrap();
            if let Some(neighbors) = graph.get(&t) {
                for n in neighbors {
                    if !visited.contains(n) {
                        visited.insert(n.clone());
                        frontier.push_back(n.clone());
                    }
                }
            }
        }
        depth += 1;
    }

    // Check whether we ran out of depth before exhausting the graph.
    if depth == ALIAS_CLOSURE_MAX_DEPTH && !frontier.is_empty() {
        if let Some(ref mut cb) = opts.on_depth_exceeded {
            cb(query_type);
        }
    }

    let mut result: Vec<String> = visited.into_iter().collect();
    result.sort();
    result
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema_pack::manifest::{
        PageTypeDefinition, PackPrimitive, SchemaPackManifest,
    };

    fn make_manifest(page_types: Vec<PageTypeDefinition>) -> SchemaPackManifest {
        SchemaPackManifest {
            name: "test-pack".into(),
            version: "1.0.0".into(),
            page_types,
            ..Default::default()
        }
    }

    // ---- build_alias_graph -----------------------------------------------

    #[test]
    fn empty_manifest_no_edges() {
        let m = make_manifest(vec![]);
        let g = build_alias_graph(&m).unwrap();
        assert!(g.is_empty());
    }

    #[test]
    fn no_aliases_no_edges() {
        let m = make_manifest(vec![PageTypeDefinition {
            name: "note".into(),
            primitive: PackPrimitive::Concept,
            ..Default::default()
        }]);
        let g = build_alias_graph(&m).unwrap();
        // "note" exists as a key with empty neighbors
        assert_eq!(g.get("note").map(|s| s.len()), Some(0));
    }

    #[test]
    fn symmetric_edge_per_declaration() {
        // researcher declares aliases: [person]
        let m = make_manifest(vec![PageTypeDefinition {
            name: "researcher".into(),
            primitive: PackPrimitive::Entity,
            aliases: vec!["person".into()],
            ..Default::default()
        }]);
        let g = build_alias_graph(&m).unwrap();
        // researcher → person
        assert!(g.get("researcher").unwrap().contains("person"));
        // person → researcher (symmetric)
        assert!(g.get("person").unwrap().contains("researcher"));
    }

    #[test]
    fn isolated_type_not_in_closure() {
        // adversary-profile declares NO aliases
        let m = make_manifest(vec![
            PageTypeDefinition {
                name: "person".into(),
                primitive: PackPrimitive::Entity,
                aliases: vec!["researcher".into()],
                ..Default::default()
            },
            PageTypeDefinition {
                name: "adversary-profile".into(),
                primitive: PackPrimitive::Entity,
                aliases: vec![],
                ..Default::default()
            },
        ]);
        let g = build_alias_graph(&m).unwrap();
        let closure = expand_closure("person", &g, &mut ExpandClosureOpts::default());
        assert!(closure.contains(&"person".to_string()));
        assert!(closure.contains(&"researcher".to_string()));
        // adversary-profile should NOT be in person's closure
        assert!(!closure.contains(&"adversary-profile".to_string()));
    }

    // ---- Cycles ----------------------------------------------------------

    #[test]
    fn cycle_detection_rejects_triangle() {
        let m = make_manifest(vec![
            PageTypeDefinition {
                name: "A".into(),
                primitive: PackPrimitive::Entity,
                aliases: vec!["B".into()],
                ..Default::default()
            },
            PageTypeDefinition {
                name: "B".into(),
                primitive: PackPrimitive::Entity,
                aliases: vec!["C".into()],
                ..Default::default()
            },
            PageTypeDefinition {
                name: "C".into(),
                primitive: PackPrimitive::Entity,
                aliases: vec!["A".into()],
                ..Default::default()
            },
        ]);
        let err = build_alias_graph(&m).unwrap_err();
        assert!(err.path.len() >= 3, "cycle path should include at least 3 nodes");
        assert!(err.to_string().contains("→"), "error message should show cycle path");
    }

    #[test]
    fn direct_bidirectional_is_not_a_cycle() {
        // A declares B → symmetric A↔B. This is NOT a cycle.
        let m = make_manifest(vec![PageTypeDefinition {
            name: "A".into(),
            primitive: PackPrimitive::Entity,
            aliases: vec!["B".into()],
            ..Default::default()
        }]);
        let g = build_alias_graph(&m).unwrap();
        assert!(g.get("A").unwrap().contains("B"));
        assert!(g.get("B").unwrap().contains("A"));
    }

    // ---- expand_closure --------------------------------------------------

    #[test]
    fn closure_includes_self() {
        let m = make_manifest(vec![]);
        let g = build_alias_graph(&m).unwrap();
        let c = expand_closure("solo", &g, &mut ExpandClosureOpts::default());
        assert_eq!(c, vec!["solo"]);
    }

    #[test]
    fn closure_is_sorted() {
        let m = make_manifest(vec![PageTypeDefinition {
            name: "zebra".into(),
            primitive: PackPrimitive::Entity,
            aliases: vec!["alpha".into()],
            ..Default::default()
        }]);
        let g = build_alias_graph(&m).unwrap();
        let c = expand_closure("zebra", &g, &mut ExpandClosureOpts::default());
        assert_eq!(c, vec!["alpha", "zebra"]);
    }

    #[test]
    fn transitive_closure_depth_3() {
        // A → B → C → D, all declared
        let m = make_manifest(vec![
            PageTypeDefinition {
                name: "A".into(),
                primitive: PackPrimitive::Entity,
                aliases: vec!["B".into()],
                ..Default::default()
            },
            PageTypeDefinition {
                name: "B".into(),
                primitive: PackPrimitive::Entity,
                aliases: vec!["C".into()],
                ..Default::default()
            },
            PageTypeDefinition {
                name: "C".into(),
                primitive: PackPrimitive::Entity,
                aliases: vec!["D".into()],
                ..Default::default()
            },
        ]);
        let g = build_alias_graph(&m).unwrap();
        let c = expand_closure("A", &g, &mut ExpandClosureOpts::default());
        assert_eq!(c, vec!["A", "B", "C", "D"]);
    }

    #[test]
    fn depth_cap_triggers_callback() {
        use std::cell::Cell;
        use std::rc::Rc;
        // Chain A→B→C→D→E→F (depth 5) — should hit cap at depth 4
        let mut types = vec![];
        for (i, name) in ["A", "B", "C", "D", "E", "F"].iter().enumerate() {
            let alias = if i + 1 < 6 {
                vec![["B", "C", "D", "E", "F"][i].to_string()]
            } else {
                vec![]
            };
            types.push(PageTypeDefinition {
                name: name.to_string(),
                primitive: PackPrimitive::Concept,
                aliases: alias,
                ..Default::default()
            });
        }
        let m = make_manifest(types);
        let g = build_alias_graph(&m).unwrap();

        let exceeded = Rc::new(Cell::new(false));
        let mut opts = ExpandClosureOpts {
            on_depth_exceeded: Some(Box::new({
                let exceeded = Rc::clone(&exceeded);
                move |_qt: &str| {
                    exceeded.set(true);
                }
            })),
        };
        expand_closure("A", &g, &mut opts);
        assert!(exceeded.get(), "depth cap callback should fire for 6-node chain");
    }
}
