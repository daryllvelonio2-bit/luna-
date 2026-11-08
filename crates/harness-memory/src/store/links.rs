//! Within-domain entity co-occurrence links.
//!
//! Direct port of MemPalace's `hallways.py` and `links` topology.
//! Stores within-domain entity connections in `links/<domain>.links.md`.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::PathBuf;

use chrono::Utc;
use harness_core::HarnessError;
use serde::{Deserialize, Serialize};

use super::atomic;
use super::frontmatter;
use super::ids::{canonical_entity_name, slugify};
use super::Record;

fn links_err(msg: impl Into<String>) -> HarnessError {
    HarnessError::MemoryError(msg.into())
}

/// Generic stopwords that should not form links or crosslinks.
pub const GENERIC_STOPWORDS: &[&str] = &[
    "content",
    "thinking",
    "general",
    "file",
    "data",
    "test",
    "record",
    "true",
    "false",
    "none",
    "null",
    "this",
    "that",
    "code",
    "system",
    "user",
    "assistant",
    "info",
    "item",
    "default",
    "value",
    "text",
];

pub fn is_generic_entity(name: &str) -> bool {
    let lower = name.trim().to_lowercase();
    if lower.len() < 3 {
        return true;
    }
    GENERIC_STOPWORDS.iter().any(|&sw| lower == sw)
}

/// One link representing entity co-occurrence inside a domain.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DomainLink {
    pub domain: String,
    pub entity_a: String,
    pub entity_b: String,
    pub count: usize,
    pub records: Vec<String>,
}

impl DomainLink {
    pub fn new(
        domain: &str,
        entity_a: &str,
        entity_b: &str,
        count: usize,
        records: Vec<String>,
    ) -> Self {
        let (a, b) = if entity_a.to_lowercase() <= entity_b.to_lowercase() {
            (entity_a.trim().to_string(), entity_b.trim().to_string())
        } else {
            (entity_b.trim().to_string(), entity_a.trim().to_string())
        };
        Self {
            domain: slugify(domain),
            entity_a: a,
            entity_b: b,
            count,
            records,
        }
    }

    pub fn render_line(&self) -> String {
        let recs_json = serde_json::to_string(&self.records).unwrap_or_else(|_| "[]".to_string());
        format!(
            "- a: {} | b: {} | count: {} | records: {}",
            self.entity_a, self.entity_b, self.count, recs_json
        )
    }

    pub fn parse_line(domain: &str, line: &str) -> Option<Self> {
        let trimmed = line.trim();
        let content = trimmed.strip_prefix("- ")?;

        let mut a = String::new();
        let mut b = String::new();
        let mut count = 0usize;
        let mut records = Vec::new();

        for part in content.split('|') {
            let p = part.trim();
            if let Some(v) = p.strip_prefix("a:") {
                a = v.trim().to_string();
            } else if let Some(v) = p.strip_prefix("b:") {
                b = v.trim().to_string();
            } else if let Some(v) = p.strip_prefix("count:") {
                count = v.trim().parse().unwrap_or(0);
            } else if let Some(v) = p.strip_prefix("records:") {
                let rec_str = v.trim();
                if let Ok(parsed) = serde_json::from_str::<Vec<String>>(rec_str) {
                    records = parsed;
                } else {
                    // Try parsing comma-separated bracketed list: [rec_1, rec_2]
                    let inner = rec_str.trim_matches(['[', ']']);
                    for item in inner.split(',') {
                        let it = item.trim().trim_matches('"');
                        if !it.is_empty() {
                            records.push(it.to_string());
                        }
                    }
                }
            }
        }

        if a.is_empty() || b.is_empty() {
            return None;
        }

        Some(Self::new(domain, &a, &b, count, records))
    }
}

pub struct LinksStore {
    pub root: PathBuf,
}

impl LinksStore {
    pub fn new(store_root: PathBuf) -> Self {
        Self { root: store_root }
    }

    pub fn links_dir(&self) -> PathBuf {
        self.root.join("links")
    }

    pub fn link_file_path(&self, domain: &str) -> PathBuf {
        self.links_dir()
            .join(format!("{}.links.md", slugify(domain)))
    }

    pub fn ensure_dirs(&self) -> Result<(), HarnessError> {
        fs::create_dir_all(self.links_dir()).map_err(|e| links_err(e.to_string()))?;
        Ok(())
    }

    pub fn get_links_for_domain(&self, domain: &str) -> Result<Vec<DomainLink>, HarnessError> {
        let path = self.link_file_path(domain);
        if !path.is_file() {
            return Ok(Vec::new());
        }
        let text = fs::read_to_string(&path).map_err(|e| links_err(e.to_string()))?;
        let (yaml, body) = frontmatter::split(&text)
            .ok_or_else(|| links_err("missing front matter in links file"))?;
        let val = frontmatter::parse(yaml).map_err(|e| links_err(e.to_string()))?;
        let dom = frontmatter::string(&val, "domain").unwrap_or_else(|| domain.to_string());

        let mut links = Vec::new();
        for line in body.lines() {
            if let Some(link) = DomainLink::parse_line(&dom, line) {
                links.push(link);
            }
        }
        Ok(links)
    }

    pub fn save_links_for_domain(
        &self,
        domain: &str,
        links: &[DomainLink],
    ) -> Result<(), HarnessError> {
        self.ensure_dirs()?;
        let path = self.link_file_path(domain);
        let yaml = format!(
            "domain: {}\nbuilt_at: {}",
            slugify(domain),
            Utc::now().to_rfc3339()
        );
        let mut body = String::new();
        for link in links {
            body.push_str(&link.render_line());
            body.push('\n');
        }
        let doc = frontmatter::render(&yaml, &body);
        atomic::write_str(&path, &doc).map_err(|e| links_err(e.to_string()))?;
        Ok(())
    }

    pub fn list_all_links(&self) -> Result<Vec<DomainLink>, HarnessError> {
        let dir = self.links_dir();
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut all = Vec::new();
        for entry in fs::read_dir(dir).map_err(|e| links_err(e.to_string()))? {
            let entry = entry.map_err(|e| links_err(e.to_string()))?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("md") {
                let stem = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default();
                let domain = stem.strip_suffix(".links").unwrap_or(stem);
                let links = self.get_links_for_domain(domain)?;
                all.extend(links);
            }
        }
        Ok(all)
    }

    /// Compute within-domain entity co-occurrence from active records.
    pub fn compute_links_for_domain(
        &self,
        domain: &str,
        records: &[Record],
    ) -> Result<Vec<DomainLink>, HarnessError> {
        let mut co_occur: HashMap<(String, String), (usize, BTreeSet<String>)> = HashMap::new();

        for r in records {
            if slugify(&r.domain) != slugify(domain) || !r.is_active {
                continue;
            }

            let mut ent_set = BTreeSet::new();
            for ent in &r.entities {
                if !is_generic_entity(ent) {
                    ent_set.insert(canonical_entity_name(ent));
                }
            }
            if !r.subject.trim().is_empty() && !is_generic_entity(&r.subject) {
                ent_set.insert(canonical_entity_name(&r.subject));
            }
            if !r.object.trim().is_empty() && r.object.len() < 50 && !is_generic_entity(&r.object) {
                ent_set.insert(canonical_entity_name(&r.object));
            }

            let ent_vec: Vec<String> = ent_set.into_iter().collect();
            if ent_vec.len() >= 2 {
                for i in 0..ent_vec.len() {
                    for j in (i + 1)..ent_vec.len() {
                        let a = &ent_vec[i];
                        let b = &ent_vec[j];
                        let pair = if a <= b {
                            (a.clone(), b.clone())
                        } else {
                            (b.clone(), a.clone())
                        };
                        let entry = co_occur.entry(pair).or_insert((0, BTreeSet::new()));
                        entry.0 += 1;
                        entry.1.insert(r.id.clone());
                    }
                }
            }
        }

        let mut links = Vec::new();
        for ((a, b), (count, rec_ids)) in co_occur {
            links.push(DomainLink::new(
                domain,
                &a,
                &b,
                count,
                rec_ids.into_iter().collect(),
            ));
        }

        links.sort_by(|x, y| {
            y.count
                .cmp(&x.count)
                .then_with(|| x.entity_a.cmp(&y.entity_a))
        });
        self.save_links_for_domain(domain, &links)?;
        Ok(links)
    }

    pub fn delete_link(
        &self,
        domain: &str,
        entity_a: &str,
        entity_b: &str,
    ) -> Result<bool, HarnessError> {
        let mut links = self.get_links_for_domain(domain)?;
        let initial_len = links.len();
        let target = DomainLink::new(domain, entity_a, entity_b, 0, Vec::new());
        links.retain(|l| !(l.entity_a == target.entity_a && l.entity_b == target.entity_b));

        if links.len() != initial_len {
            self.save_links_for_domain(domain, &links)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn prune_links(
        &self,
        domain: Option<&str>,
        min_count: usize,
    ) -> Result<usize, HarnessError> {
        let domains: Vec<String> = if let Some(d) = domain {
            vec![slugify(d)]
        } else {
            let mut set = BTreeSet::new();
            for link in self.list_all_links()? {
                set.insert(link.domain);
            }
            set.into_iter().collect()
        };

        let mut total_pruned = 0;
        for d in domains {
            let links = self.get_links_for_domain(&d)?;
            let count_before = links.len();
            let retained: Vec<DomainLink> = links
                .into_iter()
                .filter(|l| {
                    l.count >= min_count
                        && !is_generic_entity(&l.entity_a)
                        && !is_generic_entity(&l.entity_b)
                })
                .collect();
            let count_after = retained.len();
            if count_after != count_before {
                self.save_links_for_domain(&d, &retained)?;
                total_pruned += count_before - count_after;
            }
        }

        Ok(total_pruned)
    }
}
