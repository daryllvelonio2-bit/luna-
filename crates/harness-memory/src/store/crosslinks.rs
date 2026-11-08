//! Cross-domain links: proposal, review, approval, and pruning workflows.
//!
//! Direct port of MemPalace's `tunnels_tool.py` and `palace_graph.py` tunnels.
//! Cross-links connect entities between domains/topics, proposed via the
//! weaker-side strength rule, reviewed in `crosslinks/proposal.md`, and
//! materialized into canonical `crosslinks/<entity>.crosslinks.md`.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::PathBuf;

use chrono::Utc;
use harness_core::HarnessError;
use serde::{Deserialize, Serialize};

use super::atomic;
use super::frontmatter;
use super::ids::{canonical_entity_name, entity_id_for_name, slugify};
use super::links::is_generic_entity;
use super::Record;

fn cross_err(msg: impl Into<String>) -> HarnessError {
    HarnessError::MemoryError(msg.into())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CrossLink {
    pub entity: String,
    pub from_endpoint: String,
    pub to_endpoint: String,
    pub label: String,
    pub strength: usize,
    pub status: String, // "proposed", "approved", "rejected"
}

impl CrossLink {
    pub fn new(
        entity: &str,
        from_endpoint: &str,
        to_endpoint: &str,
        label: Option<&str>,
        strength: usize,
        status: &str,
    ) -> Self {
        let (from, to) = if from_endpoint <= to_endpoint {
            (
                from_endpoint.trim().to_string(),
                to_endpoint.trim().to_string(),
            )
        } else {
            (
                to_endpoint.trim().to_string(),
                from_endpoint.trim().to_string(),
            )
        };
        let lbl = label
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| entity.trim().to_string());
        Self {
            entity: entity.trim().to_string(),
            from_endpoint: from,
            to_endpoint: to,
            label: lbl,
            strength,
            status: status.trim().to_string(),
        }
    }

    pub fn render_line(&self) -> String {
        format!(
            "- from: {} | to: {} | label: {} | strength: {} | status: {}",
            self.from_endpoint, self.to_endpoint, self.label, self.strength, self.status
        )
    }

    pub fn parse_line(entity: &str, line: &str) -> Option<Self> {
        let trimmed = line.trim();
        let content = trimmed.strip_prefix("- ")?;

        let mut from = String::new();
        let mut to = String::new();
        let mut label = String::new();
        let mut strength = 0usize;
        let mut status = "approved".to_string();

        for part in content.split('|') {
            let p = part.trim();
            if let Some(v) = p.strip_prefix("from:") {
                from = v.trim().to_string();
            } else if let Some(v) = p.strip_prefix("to:") {
                to = v.trim().to_string();
            } else if let Some(v) = p.strip_prefix("label:") {
                label = v.trim().to_string();
            } else if let Some(v) = p.strip_prefix("strength:") {
                strength = v.trim().parse().unwrap_or(0);
            } else if let Some(v) = p.strip_prefix("status:") {
                status = v.trim().to_string();
            }
        }

        if from.is_empty() || to.is_empty() {
            return None;
        }

        Some(Self::new(
            entity,
            &from,
            &to,
            if label.is_empty() { None } else { Some(&label) },
            strength,
            &status,
        ))
    }
}

pub struct CrossLinksStore {
    pub root: PathBuf,
}

impl CrossLinksStore {
    pub fn new(store_root: PathBuf) -> Self {
        Self { root: store_root }
    }

    pub fn crosslinks_dir(&self) -> PathBuf {
        self.root.join("crosslinks")
    }

    pub fn proposal_path(&self) -> PathBuf {
        self.crosslinks_dir().join("proposal.md")
    }

    pub fn crosslink_file_path(&self, entity: &str) -> PathBuf {
        let eid = entity_id_for_name(entity);
        self.crosslinks_dir()
            .join(format!("{}.crosslinks.md", slugify(&eid)))
    }

    pub fn ensure_dirs(&self) -> Result<(), HarnessError> {
        fs::create_dir_all(self.crosslinks_dir()).map_err(|e| cross_err(e.to_string()))?;
        Ok(())
    }

    pub fn get_crosslinks_for_entity(&self, entity: &str) -> Result<Vec<CrossLink>, HarnessError> {
        let path = self.crosslink_file_path(entity);
        if !path.is_file() {
            return Ok(Vec::new());
        }
        let text = fs::read_to_string(&path).map_err(|e| cross_err(e.to_string()))?;
        let (yaml, body) = frontmatter::split(&text)
            .ok_or_else(|| cross_err("missing front matter in crosslink file"))?;
        let val = frontmatter::parse(yaml).map_err(|e| cross_err(e.to_string()))?;
        let ent = frontmatter::string(&val, "entity").unwrap_or_else(|| entity.to_string());

        let mut links = Vec::new();
        for line in body.lines() {
            if let Some(link) = CrossLink::parse_line(&ent, line) {
                links.push(link);
            }
        }
        Ok(links)
    }

    pub fn save_crosslinks_for_entity(
        &self,
        entity: &str,
        links: &[CrossLink],
    ) -> Result<(), HarnessError> {
        self.ensure_dirs()?;
        let path = self.crosslink_file_path(entity);
        if links.is_empty() {
            if path.is_file() {
                let _ = fs::remove_file(&path);
            }
            return Ok(());
        }
        let yaml = format!(
            "entity: {}\nupdated_at: {}",
            entity,
            Utc::now().to_rfc3339()
        );
        let mut body = String::new();
        for link in links {
            body.push_str(&link.render_line());
            body.push('\n');
        }
        let doc = frontmatter::render(&yaml, &body);
        atomic::write_str(&path, &doc).map_err(|e| cross_err(e.to_string()))?;
        Ok(())
    }

    pub fn list_all_crosslinks(&self) -> Result<Vec<CrossLink>, HarnessError> {
        let dir = self.crosslinks_dir();
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut all = Vec::new();
        for entry in fs::read_dir(dir).map_err(|e| cross_err(e.to_string()))? {
            let entry = entry.map_err(|e| cross_err(e.to_string()))?;
            let path = entry.path();
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            if name == "proposal.md" || !name.ends_with(".crosslinks.md") {
                continue;
            }
            let text = fs::read_to_string(&path).map_err(|e| cross_err(e.to_string()))?;
            if let Some((yaml, body)) = frontmatter::split(&text) {
                if let Ok(val) = frontmatter::parse(yaml) {
                    let ent = frontmatter::string(&val, "entity").unwrap_or_default();
                    for line in body.lines() {
                        if let Some(link) = CrossLink::parse_line(&ent, line) {
                            all.push(link);
                        }
                    }
                }
            }
        }
        Ok(all)
    }

    /// Propose cross-links across domains using MemPalace's weaker-side strength rule.
    /// Writes `crosslinks/proposal.md` for human review.
    pub fn propose_crosslinks(
        &self,
        records: &[Record],
        max_proposals: usize,
    ) -> Result<Vec<CrossLink>, HarnessError> {
        self.ensure_dirs()?;

        // Count appearances of each entity in each domain/topic endpoint
        // endpoint = "domain/topic"
        let mut entity_endpoints: HashMap<String, HashMap<String, usize>> = HashMap::new();

        for r in records {
            if !r.is_active {
                continue;
            }
            let endpoint = format!("{}/{}", slugify(&r.domain), slugify(&r.topic));

            let mut ent_set = BTreeSet::new();
            for ent in &r.entities {
                if !is_generic_entity(ent) {
                    ent_set.insert(canonical_entity_name(ent));
                }
            }
            if !r.subject.trim().is_empty() && !is_generic_entity(&r.subject) {
                ent_set.insert(canonical_entity_name(&r.subject));
            }

            for ent in ent_set {
                let counts = entity_endpoints.entry(ent).or_default();
                *counts.entry(endpoint.clone()).or_insert(0) += 1;
            }
        }

        // Existing approved links
        let existing = self.list_all_crosslinks()?;
        let mut existing_set = BTreeSet::new();
        for el in existing {
            existing_set.insert((
                canonical_entity_name(&el.entity),
                el.from_endpoint.clone(),
                el.to_endpoint.clone(),
            ));
        }

        let mut proposals = Vec::new();

        for (ent, endpoints) in entity_endpoints {
            if endpoints.len() < 2 {
                continue;
            }
            let ep_vec: Vec<(&String, &usize)> = endpoints.iter().collect();
            for i in 0..ep_vec.len() {
                for j in (i + 1)..ep_vec.len() {
                    let (ep1, c1) = ep_vec[i];
                    let (ep2, c2) = ep_vec[j];

                    let (from, to) = if ep1 <= ep2 {
                        (ep1.clone(), ep2.clone())
                    } else {
                        (ep2.clone(), ep1.clone())
                    };

                    if existing_set.contains(&(ent.clone(), from.clone(), to.clone())) {
                        continue;
                    }

                    // Weaker side rule: strength = min(c1, c2)
                    let strength = (*c1).min(*c2);
                    proposals.push(CrossLink::new(
                        &ent,
                        &from,
                        &to,
                        Some(&ent),
                        strength,
                        "proposed",
                    ));
                }
            }
        }

        // Rank by strength descending
        proposals.sort_by(|a, b| {
            b.strength
                .cmp(&a.strength)
                .then_with(|| a.entity.cmp(&b.entity))
        });
        proposals.truncate(max_proposals);

        // Write proposal file
        let yaml = format!("status: proposed\nupdated_at: {}", Utc::now().to_rfc3339());
        let mut body = String::new();
        for p in &proposals {
            body.push_str(&p.render_line());
            body.push('\n');
        }

        let doc = frontmatter::render(&yaml, &body);
        atomic::write_str(&self.proposal_path(), &doc).map_err(|e| cross_err(e.to_string()))?;

        Ok(proposals)
    }

    /// Read `crosslinks/proposal.md`.
    pub fn read_proposal(&self) -> Result<Vec<CrossLink>, HarnessError> {
        let path = self.proposal_path();
        if !path.is_file() {
            return Ok(Vec::new());
        }
        let text = fs::read_to_string(&path).map_err(|e| cross_err(e.to_string()))?;
        let (_yaml, body) = frontmatter::split(&text)
            .ok_or_else(|| cross_err("missing front matter in proposal"))?;
        let mut proposals = Vec::new();
        for line in body.lines() {
            if let Some(link) = CrossLink::parse_line("", line) {
                // Label or entity is recovered from line
                let mut ent = link.label.clone();
                if ent.is_empty() {
                    ent = link.entity.clone();
                }
                let mut fixed = link;
                fixed.entity = ent;
                proposals.push(fixed);
            }
        }
        Ok(proposals)
    }

    /// Apply approved crosslinks from `crosslinks/proposal.md` into canonical files.
    pub fn apply_crosslinks(&self) -> Result<usize, HarnessError> {
        let proposals = self.read_proposal()?;
        if proposals.is_empty() {
            return Ok(0);
        }

        let mut by_entity: HashMap<String, Vec<CrossLink>> = HashMap::new();
        let mut applied_count = 0;

        for mut prop in proposals {
            // Apply links marked approved or proposed during apply
            prop.status = "approved".to_string();
            by_entity.entry(prop.entity.clone()).or_default().push(prop);
            applied_count += 1;
        }

        for (ent, new_links) in by_entity {
            let mut existing = self.get_crosslinks_for_entity(&ent)?;
            for nl in new_links {
                if !existing
                    .iter()
                    .any(|e| e.from_endpoint == nl.from_endpoint && e.to_endpoint == nl.to_endpoint)
                {
                    existing.push(nl);
                }
            }
            self.save_crosslinks_for_entity(&ent, &existing)?;
        }

        // Clean up proposal file after apply
        let _ = fs::remove_file(self.proposal_path());

        Ok(applied_count)
    }

    /// Prune generic tokens, dangling endpoints, and duplicate reverse spellings.
    pub fn prune_crosslinks(
        &self,
        valid_endpoints: Option<&[String]>,
    ) -> Result<usize, HarnessError> {
        let all = self.list_all_crosslinks()?;
        let mut by_entity: HashMap<String, Vec<CrossLink>> = HashMap::new();
        for link in all {
            by_entity.entry(link.entity.clone()).or_default().push(link);
        }

        let mut pruned = 0;
        let endpoint_set: Option<BTreeSet<&str>> =
            valid_endpoints.map(|v| v.iter().map(String::as_str).collect());

        for (ent, links) in by_entity {
            let before = links.len();
            let mut retained = Vec::new();
            let mut seen_pairs = BTreeSet::new();

            for l in links {
                if is_generic_entity(&l.entity) || is_generic_entity(&l.label) {
                    continue;
                }
                if let Some(set) = &endpoint_set {
                    if !set.contains(l.from_endpoint.as_str())
                        || !set.contains(l.to_endpoint.as_str())
                    {
                        continue;
                    }
                }
                let pair = (l.from_endpoint.clone(), l.to_endpoint.clone());
                if seen_pairs.contains(&pair) {
                    continue;
                }
                seen_pairs.insert(pair);
                retained.push(l);
            }

            let after = retained.len();
            if after != before {
                pruned += before - after;
                self.save_crosslinks_for_entity(&ent, &retained)?;
            }
        }

        Ok(pruned)
    }
}
