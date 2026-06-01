//! Graph subgraph builders for the `gt://graph/{domain,role,depends_on,blocks,surface}`
//! resources (hq-taxon.4).
//!
//! Spec: `apps/api/docs/14-bead-taxonomy.md` §7.
//!
//! Each builder takes a flat list of [`IssueRow`] snapshots (the same shape
//! `gt://issues` returns) and produces a `{ "query": ..., "nodes": [...], "edges": [...] }`
//! payload. The JSON columns (`domain_json`/`surface_json`/`depends_on_json`)
//! are parsed lazily per row; malformed strings fall back to empty so a single
//! bad row never poisons the whole snapshot.
//!
//! Transitive traversals (`depends_on` forward, `blocks` backward) are bounded
//! by [`MAX_DEPTH`] so a cyclic or pathological graph can't make the resource
//! response unbounded.

use std::collections::{HashMap, HashSet, VecDeque};

use gt_store_dolt::IssueRow;
use serde_json::{json, Value};

/// Maximum hop count for transitive resources. Beyond this, the traversal
/// stops; the omitted nodes are signalled via the `truncated` field in the
/// payload so the operator can request a deeper walk if it ever matters.
pub const MAX_DEPTH: u32 = 16;

fn parse_array(json_str: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(json_str).unwrap_or_default()
}

fn node_value(row: &IssueRow) -> Value {
    json!({
        "id": row.id,
        "title": row.title,
        "status": row.status,
        "issue_type": row.issue_type,
        "priority": row.priority,
        "external_ref": row.external_ref,
        "domain": parse_array(&row.domain_json),
        "surface": parse_array(&row.surface_json),
        "depends_on": parse_array(&row.depends_on_json),
        "role_scope": row.role_scope,
    })
}

fn edges_from(rows: &[IssueRow], include: &HashSet<String>) -> Vec<Value> {
    let mut edges = Vec::new();
    for row in rows {
        if !include.contains(&row.id) {
            continue;
        }
        for dep in parse_array(&row.depends_on_json) {
            // Edge endpoints both have to be in the snapshot — otherwise the
            // consumer would render a dangling target with no metadata. The
            // backward / forward builders below already widen `include` to
            // capture both ends of every edge they want surfaced.
            if include.contains(&dep) {
                edges.push(json!({ "from": row.id, "to": dep }));
            }
        }
    }
    edges
}

/// Beads + epics whose `domain[]` contains `needle`. 1-level depends_on edges
/// among the returned set are included.
pub fn by_domain(rows: &[IssueRow], needle: &str) -> Value {
    let matched: Vec<&IssueRow> = rows
        .iter()
        .filter(|r| parse_array(&r.domain_json).iter().any(|d| d == needle))
        .collect();
    let ids: HashSet<String> = matched.iter().map(|r| r.id.clone()).collect();
    let nodes: Vec<Value> = matched.iter().map(|r| node_value(r)).collect();
    let edges = edges_from(rows, &ids);
    json!({
        "query": { "kind": "domain", "value": needle },
        "nodes": nodes,
        "edges": edges,
    })
}

/// Beads + epics whose `role_scope` matches. 1-level edges among the matched
/// set are included.
pub fn by_role(rows: &[IssueRow], needle: &str) -> Value {
    let matched: Vec<&IssueRow> = rows
        .iter()
        .filter(|r| r.role_scope.as_deref() == Some(needle))
        .collect();
    let ids: HashSet<String> = matched.iter().map(|r| r.id.clone()).collect();
    let nodes: Vec<Value> = matched.iter().map(|r| node_value(r)).collect();
    let edges = edges_from(rows, &ids);
    json!({
        "query": { "kind": "role", "value": needle },
        "nodes": nodes,
        "edges": edges,
    })
}

/// Beads + epics whose `surface[]` contains `needle` (exact entry match — the
/// JSON is an array of strings, this is per-entry equality, not substring).
pub fn by_surface(rows: &[IssueRow], needle: &str) -> Value {
    let matched: Vec<&IssueRow> = rows
        .iter()
        .filter(|r| parse_array(&r.surface_json).iter().any(|s| s == needle))
        .collect();
    let ids: HashSet<String> = matched.iter().map(|r| r.id.clone()).collect();
    let nodes: Vec<Value> = matched.iter().map(|r| node_value(r)).collect();
    let edges = edges_from(rows, &ids);
    json!({
        "query": { "kind": "surface", "value": needle },
        "nodes": nodes,
        "edges": edges,
    })
}

/// Forward transitive closure: every bead reachable from `start` by walking
/// `depends_on` edges. The starting bead is included if it is in `rows`; a
/// missing start surfaces as `"missing_start": true` so the consumer can tell
/// "no edges" from "bead not found".
pub fn depends_on(rows: &[IssueRow], start: &str) -> Value {
    let by_id: HashMap<&str, &IssueRow> =
        rows.iter().map(|r| (r.id.as_str(), r)).collect();
    let missing_start = !by_id.contains_key(start);

    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<(String, u32)> = VecDeque::new();
    if !missing_start {
        visited.insert(start.to_string());
        queue.push_back((start.to_string(), 0));
    }
    let mut truncated = false;

    while let Some((id, depth)) = queue.pop_front() {
        if depth >= MAX_DEPTH {
            truncated = true;
            continue;
        }
        let Some(row) = by_id.get(id.as_str()) else { continue };
        for dep in parse_array(&row.depends_on_json) {
            if visited.insert(dep.clone()) {
                queue.push_back((dep, depth + 1));
            }
        }
    }

    let nodes: Vec<Value> = visited
        .iter()
        .filter_map(|id| by_id.get(id.as_str()).map(|r| node_value(r)))
        .collect();
    let edges = edges_from(rows, &visited);

    json!({
        "query": { "kind": "depends_on", "value": start },
        "missing_start": missing_start,
        "truncated": truncated,
        "max_depth": MAX_DEPTH,
        "nodes": nodes,
        "edges": edges,
    })
}

/// Backward transitive closure: every bead that (transitively) lists `start`
/// in its `depends_on`. Mirrors [`depends_on`] but walks the reversed adjacency.
pub fn blocks(rows: &[IssueRow], start: &str) -> Value {
    let by_id: HashMap<&str, &IssueRow> =
        rows.iter().map(|r| (r.id.as_str(), r)).collect();
    let missing_start = !by_id.contains_key(start);

    // Build the reverse adjacency once so each transitive hop is O(blockers)
    // rather than O(rows) — keeps the resource snappy on larger graphs.
    let mut reverse: HashMap<String, Vec<String>> = HashMap::new();
    for row in rows {
        for dep in parse_array(&row.depends_on_json) {
            reverse.entry(dep).or_default().push(row.id.clone());
        }
    }

    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<(String, u32)> = VecDeque::new();
    visited.insert(start.to_string());
    queue.push_back((start.to_string(), 0));
    let mut truncated = false;

    while let Some((id, depth)) = queue.pop_front() {
        if depth >= MAX_DEPTH {
            truncated = true;
            continue;
        }
        if let Some(blockers) = reverse.get(&id) {
            for b in blockers {
                if visited.insert(b.clone()) {
                    queue.push_back((b.clone(), depth + 1));
                }
            }
        }
    }

    let nodes: Vec<Value> = visited
        .iter()
        .filter_map(|id| by_id.get(id.as_str()).map(|r| node_value(r)))
        .collect();
    let edges = edges_from(rows, &visited);

    json!({
        "query": { "kind": "blocks", "value": start },
        "missing_start": missing_start,
        "truncated": truncated,
        "max_depth": MAX_DEPTH,
        "nodes": nodes,
        "edges": edges,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, domain: &[&str], surface: &[&str], deps: &[&str], role: Option<&str>) -> IssueRow {
        let domain_json = serde_json::to_string(&domain.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap();
        let surface_json = serde_json::to_string(&surface.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap();
        let depends_on_json = serde_json::to_string(&deps.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap();
        IssueRow {
            id: id.to_string(),
            title: format!("title {id}"),
            status: "open".to_string(),
            priority: 1,
            issue_type: "task".to_string(),
            assignee: None,
            owner: None,
            created_at: None,
            updated_at: None,
            closed_at: None,
            external_ref: None,
            spec_id: None,
            domain_json,
            surface_json,
            depends_on_json,
            role_scope: role.map(|s| s.to_string()),
            version: 0,
        }
    }

    fn make_graph() -> Vec<IssueRow> {
        // a → b → c, plus d (no edges), and e with role_scope=sheriff
        vec![
            row("a", &["orch.merge"], &["gt-merge"], &["b"], None),
            row("b", &["orch.merge"], &["gt-merge"], &["c"], None),
            row("c", &["kernel.root"], &["gt-root"], &[], None),
            row("d", &["fe.web"], &["apps/web"], &[], None),
            row("e", &["kernel.plugin"], &[], &["a"], Some("sheriff")),
        ]
    }

    #[test]
    fn domain_filter_returns_matching_nodes_with_inner_edges() {
        let rows = make_graph();
        let v = by_domain(&rows, "orch.merge");
        let nodes: Vec<&str> = v["nodes"].as_array().unwrap().iter().map(|n| n["id"].as_str().unwrap()).collect();
        assert_eq!(nodes.len(), 2);
        assert!(nodes.contains(&"a") && nodes.contains(&"b"));
        let edges = v["edges"].as_array().unwrap();
        // a→b is inside the matched set; b→c crosses out of it and is dropped.
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0]["from"], "a");
        assert_eq!(edges[0]["to"], "b");
    }

    #[test]
    fn role_filter_keys_off_role_scope_column() {
        let rows = make_graph();
        let v = by_role(&rows, "sheriff");
        let nodes = v["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0]["id"], "e");
    }

    #[test]
    fn depends_on_walks_forward_transitive() {
        let rows = make_graph();
        let v = depends_on(&rows, "a");
        let mut ids: Vec<String> = v["nodes"]
            .as_array().unwrap()
            .iter()
            .map(|n| n["id"].as_str().unwrap().to_string())
            .collect();
        ids.sort();
        assert_eq!(ids, vec!["a", "b", "c"]);
        assert_eq!(v["truncated"], false);
        assert_eq!(v["missing_start"], false);
    }

    #[test]
    fn blocks_walks_backward_transitive() {
        let rows = make_graph();
        // c is blocked by b which is blocked by a; e blocks a.
        let v = blocks(&rows, "c");
        let mut ids: Vec<String> = v["nodes"]
            .as_array().unwrap()
            .iter()
            .map(|n| n["id"].as_str().unwrap().to_string())
            .collect();
        ids.sort();
        assert_eq!(ids, vec!["a", "b", "c", "e"]);
    }

    #[test]
    fn surface_filter_matches_exact_entries() {
        let rows = make_graph();
        let v = by_surface(&rows, "gt-merge");
        let ids: Vec<&str> = v["nodes"].as_array().unwrap().iter().map(|n| n["id"].as_str().unwrap()).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"a") && ids.contains(&"b"));
    }

    #[test]
    fn missing_start_signalled() {
        let rows = make_graph();
        let v = depends_on(&rows, "nonexistent");
        assert_eq!(v["missing_start"], true);
        assert_eq!(v["nodes"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn cycle_does_not_blow_traversal() {
        // a → b → a — pathological after .2 ships, but stay safe regardless.
        let rows = vec![
            row("a", &["orch.merge"], &[], &["b"], None),
            row("b", &["orch.merge"], &[], &["a"], None),
        ];
        let v = depends_on(&rows, "a");
        assert_eq!(v["nodes"].as_array().unwrap().len(), 2);
        assert_eq!(v["truncated"], false);
    }
}
