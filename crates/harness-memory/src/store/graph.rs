//! Graph index and traversal: BFS walking across domain topologies and cross-link tunnels.
//!
//! Direct port of MemPalace's `palace_graph.py:traverse` and `find_tunnels`.
//! Rebuilds `search_index/graph.json` from records, within-domain links, and crosslinks.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::fs;
use std::path::PathBuf;

use harness_core::HarnessError;
use serde::{Deserialize, Serialize};

use super::atomic;
use super::crosslinks::CrossLink;
use super::ids::slugify;
use super::links::DomainLink;
use super::Record;

fn graph_err(msg: impl Into<String>) -> HarnessError {
    HarnessError::MemoryError(msg.into())
}

/// One node in the unified memory graph.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphNode {
    pub name: String,
    pub domains: Vec<String>,
    pub topics: Vec<String>,
    pub count: usize,
}

/// One edge in the unified memory graph.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphEdge {
    pub from: String,
    pub to: String,
    pub bridge: String,
    pub weight: usize,
}

/// Serialized representation of `search_index/graph.json`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GraphData {
    pub nodes: BTreeMap<String, GraphNode>,
    pub edges: Vec<GraphEdge>,
}

/// One step/result in graph traversal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TraversalStep {
    pub node: String,
    pub domains: Vec<String>,
    pub topics: Vec<String>,
    pub count: usize,
    pub hop: usize,
    pub connected_via: Option<String>,
}

/// Tunnel bridge between two domains.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TunnelBridge {
    pub bridge_entity: String,
    pub domain_a: String,
    pub domain_b: String,
    pub strength: usize,
}

pub struct GraphIndex {
    pub root: PathBuf,
}

impl GraphIndex {
    pub fn new(store_root: PathBuf) -> Self {
        Self { root: store_root }
    }

    pub fn index_path(&self) -> PathBuf {
        self.root.join("search_index").join("graph.json")
    }

    /// Load or build `search_index/graph.json`.
    pub fn load_graph(&self) -> Result<GraphData, HarnessError> {
        let path = self.index_path();
        if path.is_file() {
            if let Ok(text) = fs::read_to_string(&path) {
                if let Ok(data) = serde_json::from_str::<GraphData>(&text) {
                    return Ok(data);
                }
            }
        }
        // Fallback: build an empty graph
        Ok(GraphData::default())
    }

    /// Build graph from active records, links, and cross-links, and write `search_index/graph.json`.
    pub fn build_and_save(
        &self,
        records: &[Record],
        links: &[DomainLink],
        crosslinks: &[CrossLink],
    ) -> Result<GraphData, HarnessError> {
        let mut nodes: BTreeMap<String, GraphNode> = BTreeMap::new();
        let mut edges: Vec<GraphEdge> = Vec::new();

        // 1. Ingest records as nodes and topic anchors
        for r in records {
            if !r.is_active {
                continue;
            }
            let topic_node = format!("{}/{}", slugify(&r.domain), slugify(&r.topic));
            let entry = nodes
                .entry(topic_node.clone())
                .or_insert_with(|| GraphNode {
                    name: topic_node.clone(),
                    domains: vec![slugify(&r.domain)],
                    topics: vec![slugify(&r.topic)],
                    count: 0,
                });
            entry.count += 1;

            // Also entity nodes
            for ent in &r.entities {
                let ent_entry = nodes.entry(ent.clone()).or_insert_with(|| GraphNode {
                    name: ent.clone(),
                    domains: Vec::new(),
                    topics: Vec::new(),
                    count: 0,
                });
                ent_entry.count += 1;
                if !ent_entry.domains.contains(&slugify(&r.domain)) {
                    ent_entry.domains.push(slugify(&r.domain));
                }
                if !ent_entry.topics.contains(&slugify(&r.topic)) {
                    ent_entry.topics.push(slugify(&r.topic));
                }

                // Edge between entity and topic
                edges.push(GraphEdge {
                    from: ent.clone(),
                    to: topic_node.clone(),
                    bridge: "topic_member".to_string(),
                    weight: 1,
                });
            }
        }

        // 2. Ingest within-domain links
        for l in links {
            edges.push(GraphEdge {
                from: l.entity_a.clone(),
                to: l.entity_b.clone(),
                bridge: format!("co_occur:{}", l.domain),
                weight: l.count,
            });
        }

        // 3. Ingest cross-links (tunnels)
        for cl in crosslinks {
            if cl.status == "approved" {
                edges.push(GraphEdge {
                    from: cl.from_endpoint.clone(),
                    to: cl.to_endpoint.clone(),
                    bridge: cl.label.clone(),
                    weight: cl.strength,
                });
                // Connect bridging entity to both endpoints
                edges.push(GraphEdge {
                    from: cl.entity.clone(),
                    to: cl.from_endpoint.clone(),
                    bridge: "crosslink_bridge".to_string(),
                    weight: cl.strength,
                });
                edges.push(GraphEdge {
                    from: cl.entity.clone(),
                    to: cl.to_endpoint.clone(),
                    bridge: "crosslink_bridge".to_string(),
                    weight: cl.strength,
                });
            }
        }

        let graph_data = GraphData { nodes, edges };
        let json =
            serde_json::to_string_pretty(&graph_data).map_err(|e| graph_err(e.to_string()))?;
        fs::create_dir_all(self.root.join("search_index")).map_err(|e| graph_err(e.to_string()))?;
        atomic::write_str(&self.index_path(), &json).map_err(|e| graph_err(e.to_string()))?;

        Ok(graph_data)
    }

    /// Walk the graph from a starting room/entity/topic using BFS.
    /// Direct port of MemPalace's `palace_graph.py:traverse`.
    ///
    /// Returns connected nodes sorted by (hop, -count), capped at 50.
    pub fn traverse(
        &self,
        start_node: &str,
        max_hops: usize,
    ) -> Result<Vec<TraversalStep>, HarnessError> {
        let graph = self.load_graph()?;
        let start = start_node.trim();

        // Build adjacency map (bidirectional)
        let mut adj: HashMap<&str, Vec<(&str, &str, usize)>> = HashMap::new();
        for edge in &graph.edges {
            adj.entry(edge.from.as_str()).or_default().push((
                edge.to.as_str(),
                edge.bridge.as_str(),
                edge.weight,
            ));
            adj.entry(edge.to.as_str()).or_default().push((
                edge.from.as_str(),
                edge.bridge.as_str(),
                edge.weight,
            ));
        }

        let node_data = graph
            .nodes
            .get(start)
            .cloned()
            .unwrap_or_else(|| GraphNode {
                name: start.to_string(),
                domains: Vec::new(),
                topics: Vec::new(),
                count: 1,
            });

        let mut results = vec![TraversalStep {
            node: start.to_string(),
            domains: node_data.domains,
            topics: node_data.topics,
            count: node_data.count,
            hop: 0,
            connected_via: None,
        }];

        let mut visited = BTreeSet::new();
        visited.insert(start.to_lowercase());

        let mut queue: VecDeque<(&str, usize)> = VecDeque::new();
        queue.push_back((start, 0));

        let bound_hops = max_hops.clamp(1, 10);

        while let Some((curr, depth)) = queue.pop_front() {
            if depth >= bound_hops {
                continue;
            }

            if let Some(neighbors) = adj.get(curr) {
                for (neighbor, bridge, _weight) in neighbors {
                    let key = neighbor.to_lowercase();
                    if visited.contains(&key) {
                        continue;
                    }
                    visited.insert(key);

                    let data = graph
                        .nodes
                        .get(*neighbor)
                        .cloned()
                        .unwrap_or_else(|| GraphNode {
                            name: neighbor.to_string(),
                            domains: Vec::new(),
                            topics: Vec::new(),
                            count: 1,
                        });

                    results.push(TraversalStep {
                        node: neighbor.to_string(),
                        domains: data.domains,
                        topics: data.topics,
                        count: data.count,
                        hop: depth + 1,
                        connected_via: Some(bridge.to_string()),
                    });

                    if depth + 1 < bound_hops {
                        queue.push_back((neighbor, depth + 1));
                    }
                }
            }
        }

        // Sort by relevance (hop distance, then count descending), capped at 50
        results.sort_by(|a, b| a.hop.cmp(&b.hop).then_with(|| b.count.cmp(&a.count)));
        results.truncate(50);

        Ok(results)
    }

    /// Find bridge entities connecting two domains or topics.
    /// Direct port of MemPalace's `palace_graph.py:find_tunnels`.
    pub fn find_tunnels(
        &self,
        domain_a: Option<&str>,
        domain_b: Option<&str>,
    ) -> Result<Vec<TunnelBridge>, HarnessError> {
        let graph = self.load_graph()?;
        let da = domain_a.map(slugify);
        let db = domain_b.map(slugify);

        let matches_filter = |ep: &str, filter: Option<&String>| -> bool {
            let Some(f) = filter else {
                return true;
            };
            ep.eq_ignore_ascii_case(f)
                || ep.starts_with(&format!("{f}/"))
                || ep.split('/').any(|part| part.eq_ignore_ascii_case(f))
        };

        let mut tunnels = Vec::new();
        let mut seen = BTreeSet::new();

        // 1. Bridges from crosslink edges
        for edge in &graph.edges {
            if edge.bridge != "topic_member"
                && edge.bridge != "crosslink_bridge"
                && !edge.bridge.starts_with("co_occur:")
            {
                let from = &edge.from;
                let to = &edge.to;
                let matches_a =
                    matches_filter(from, da.as_ref()) || matches_filter(to, da.as_ref());
                let matches_b =
                    matches_filter(from, db.as_ref()) || matches_filter(to, db.as_ref());
                if matches_a && matches_b {
                    let key = (edge.bridge.clone(), from.clone(), to.clone());
                    if !seen.contains(&key) {
                        seen.insert(key);
                        tunnels.push(TunnelBridge {
                            bridge_entity: edge.bridge.clone(),
                            domain_a: from.clone(),
                            domain_b: to.clone(),
                            strength: edge.weight,
                        });
                    }
                }
            }
        }

        // 2. Bridges from multi-domain or multi-topic nodes
        for (name, node) in &graph.nodes {
            if node.domains.len() >= 2 {
                for i in 0..node.domains.len() {
                    for j in (i + 1)..node.domains.len() {
                        let dom_a = &node.domains[i];
                        let dom_b = &node.domains[j];
                        let matches_a = matches_filter(dom_a, da.as_ref())
                            || matches_filter(dom_b, da.as_ref());
                        let matches_b = matches_filter(dom_a, db.as_ref())
                            || matches_filter(dom_b, db.as_ref());
                        if matches_a && matches_b {
                            let key = (name.clone(), dom_a.clone(), dom_b.clone());
                            if !seen.contains(&key) {
                                seen.insert(key);
                                tunnels.push(TunnelBridge {
                                    bridge_entity: name.clone(),
                                    domain_a: dom_a.clone(),
                                    domain_b: dom_b.clone(),
                                    strength: node.count,
                                });
                            }
                        }
                    }
                }
            } else if node.topics.len() >= 2 && !node.domains.is_empty() {
                let dom = &node.domains[0];
                for i in 0..node.topics.len() {
                    for j in (i + 1)..node.topics.len() {
                        let ep_a = format!("{dom}/{}", node.topics[i]);
                        let ep_b = format!("{dom}/{}", node.topics[j]);
                        let matches_a = matches_filter(&ep_a, da.as_ref())
                            || matches_filter(&ep_b, da.as_ref());
                        let matches_b = matches_filter(&ep_a, db.as_ref())
                            || matches_filter(&ep_b, db.as_ref());
                        if matches_a && matches_b {
                            let key = (name.clone(), ep_a.clone(), ep_b.clone());
                            if !seen.contains(&key) {
                                seen.insert(key);
                                tunnels.push(TunnelBridge {
                                    bridge_entity: name.clone(),
                                    domain_a: ep_a,
                                    domain_b: ep_b,
                                    strength: node.count,
                                });
                            }
                        }
                    }
                }
            }
        }

        tunnels.sort_by(|x, y| {
            y.strength
                .cmp(&x.strength)
                .then_with(|| x.bridge_entity.cmp(&y.bridge_entity))
        });
        Ok(tunnels)
    }
}
