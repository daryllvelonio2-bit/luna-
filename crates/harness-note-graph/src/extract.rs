//! Turn one stored note into graph nodes and edges.
//!
//! Extraction is intentionally deterministic and dependency-free: no model is
//! asked what a note is about, so the same note always produces the same graph.
//! The heuristics are the ones graphify applies to Markdown corpora — headings,
//! links, tags, and inline code spans — plus a term list for the shared-concept
//! pass that only the builder can evaluate.

use std::collections::BTreeSet;

use harness_core::Note;

use crate::model::{
    file_id, heading_id, note_id, slug, symbol_id, tag_id, Confidence, Edge, Extraction, Node,
    NodeKind, Relation,
};

/// A note longer than this is still read, but only the first `MAX_TERMS` distinct
/// terms feed the shared-concept pass. Keeps a pasted manual from dominating the
/// graph with vocabulary.
const MAX_TERMS: usize = 400;

/// Inline code spans longer than this are prose or pasted output, not a symbol.
const MAX_CODE_SPAN_CHARS: usize = 96;

/// A code span with more words than this is prose in backticks, not a command or
/// identifier. Two covers `cargo test`; three covers a flag (`cargo test --all`).
const MAX_CODE_SPAN_WORDS: usize = 3;

/// At most this many cited files/symbols are recorded per note. A note that
/// names a hundred identifiers is a dump, not a reference card.
const MAX_MENTIONS: usize = 40;

/// Terms shorter than this carry too little signal to connect notes.
const MIN_TERM_CHARS: usize = 4;

/// Code and document suffixes that make a backticked token a file citation.
const FILE_SUFFIXES: &[&str] = &[
    ".rs", ".toml", ".json", ".yaml", ".yml", ".md", ".sh", ".py", ".ts", ".tsx", ".js", ".jsx",
    ".go", ".c", ".h", ".cpp", ".hpp", ".java", ".kt", ".sql", ".lock", ".cfg", ".ini",
];

/// Words too common to say anything about what a note is about. Kept small on
/// purpose: the document-frequency band in `build` is the real filter.
const STOPWORDS: &[&str] = &[
    "about", "after", "again", "against", "also", "always", "another", "because", "been", "before",
    "being", "between", "both", "could", "does", "doing", "done", "down", "each", "even", "every",
    "from", "further", "have", "having", "here", "into", "just", "like", "more", "most", "much",
    "must", "need", "never", "only", "other", "over", "same", "should", "since", "some", "such",
    "than", "that", "their", "them", "then", "there", "these", "they", "this", "those", "through",
    "under", "until", "very", "want", "were", "what", "when", "where", "which", "while", "will",
    "with", "would", "your", "yours", "note", "notes", "luna", "harness", "code", "file", "files",
    "true", "false", "null", "none", "doesn",
];

pub fn extract_note(note: &Note) -> Extraction {
    let note_slug = slug(&note.id);
    let source_file = format!("{note_slug}.md");
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut terms = BTreeSet::new();

    let page = page_node(note, &note_slug, &source_file);
    let page_id = page.id.clone();
    nodes.push(page);

    add_tag_edges(note, &page_id, &source_file, &mut nodes, &mut edges);

    if let Some(parent) = note.parent_id.as_deref().filter(|p| !p.trim().is_empty()) {
        let parent_slug = slug(parent);
        edges.push(Edge::new(
            &page_id,
            note_id(&parent_slug),
            Relation::PartOf,
            Confidence::Extracted,
        ));
    }

    let lines: Vec<&str> = note.content.lines().collect();
    let mut heading_stack: Vec<(usize, String)> = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    let mut mentions = 0usize;

    for (index, raw) in lines.iter().enumerate() {
        let line_no = index + 1;
        let line = *raw;

        if let Some((ch, len)) = fence {
            if closes_fence(line, ch, len) {
                fence = None;
            }
            continue;
        }
        if let Some(opened) = opens_fence(line) {
            fence = Some(opened);
            continue;
        }

        if add_heading(
            line,
            line_no,
            &note_slug,
            &source_file,
            &page_id,
            &mut heading_stack,
            &mut nodes,
            &mut edges,
        ) {
            continue;
        }

        scan_inline_links(
            line,
            line_no,
            &note_slug,
            &source_file,
            &page_id,
            &mut mentions,
            &mut nodes,
            &mut edges,
        );

        for term in terms_in(line) {
            if terms.len() >= MAX_TERMS {
                break;
            }
            terms.insert(term);
        }
    }

    Extraction {
        nodes,
        edges,
        terms: terms.into_iter().collect(),
    }
}

fn page_node(note: &Note, note_slug: &str, source_file: &str) -> Node {
    let mut page = Node::new(note_id(note_slug), note.title.clone(), NodeKind::Note)
        .at(source_file.to_string(), 1)
        .with("id", note.id.clone());
    if !note.summary_pointer.trim().is_empty() {
        page = page.with("summary_pointer", note.summary_pointer.trim());
    }
    if !note.tags.is_empty() {
        page = page.with("tags", note.tags.join(", "));
    }
    page
}

fn add_tag_edges(
    note: &Note,
    page_id: &str,
    source_file: &str,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    for tag in &note.tags {
        if tag.trim().is_empty() {
            continue;
        }
        let id = tag_id(tag);
        nodes.push(Node::new(id.clone(), tag.trim(), NodeKind::Tag).at(source_file.to_string(), 1));
        edges.push(Edge::new(
            page_id,
            id,
            Relation::Tagged,
            Confidence::Extracted,
        ));
    }
}

#[allow(clippy::too_many_arguments)]
fn add_heading(
    line: &str,
    line_no: usize,
    note_slug: &str,
    source_file: &str,
    page_id: &str,
    heading_stack: &mut Vec<(usize, String)>,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) -> bool {
    let Some((level, title)) = atx_heading(line) else {
        return false;
    };
    let heading = Node::new(
        heading_id(note_slug, &slug(&title), line_no),
        title.clone(),
        NodeKind::Heading,
    )
    .at(source_file.to_string(), line_no)
    .with("level", level.to_string());
    let heading_nid = heading.id.clone();

    while heading_stack
        .last()
        .is_some_and(|(parent_level, _)| *parent_level >= level)
    {
        heading_stack.pop();
    }
    let parent = heading_stack
        .last()
        .map(|(_, id)| id.clone())
        .unwrap_or_else(|| page_id.to_string());
    edges.push(Edge::new(
        parent,
        heading_nid.clone(),
        Relation::Contains,
        Confidence::Extracted,
    ));
    heading_stack.push((level, heading_nid));
    nodes.push(heading);
    true
}

#[allow(clippy::too_many_arguments)]
fn scan_inline_links(
    line: &str,
    line_no: usize,
    note_slug: &str,
    source_file: &str,
    page_id: &str,
    mentions: &mut usize,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    for target in wikilinks(line) {
        add_note_link(
            page_id,
            note_slug,
            &target,
            line_no,
            source_file,
            nodes,
            edges,
        );
    }
    for target in markdown_links(line) {
        add_markdown_link(
            page_id,
            note_slug,
            &target,
            line_no,
            source_file,
            nodes,
            edges,
        );
    }
    for span in code_spans(line) {
        if *mentions >= MAX_MENTIONS {
            break;
        }
        if add_code_span(page_id, &span, line_no, source_file, nodes, edges) {
            *mentions += 1;
        }
    }
}

fn add_note_link(
    page_id: &str,
    self_slug: &str,
    target: &str,
    line: usize,
    source_file: &str,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    let Some(target_slug) = note_target_slug(target) else {
        return;
    };
    if target_slug == self_slug {
        return;
    }
    let id = note_id(&target_slug);
    edges.push(Edge::new(
        page_id,
        &id,
        Relation::References,
        Confidence::Extracted,
    ));
    // The target note itself is added by its own extraction. This placeholder is
    // replaced during build when it exists, and becomes a `missing` node when the
    // link is broken.
    nodes.push(
        Node::new(id.clone(), target_slug.replace('-', " "), NodeKind::Note)
            .at(source_file.to_string(), line)
            .with("placeholder", "true"),
    );
}

fn add_markdown_link(
    page_id: &str,
    self_slug: &str,
    target: &str,
    line: usize,
    source_file: &str,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    let cleaned = clean_link_target(target);
    if cleaned.is_empty() || is_external(&cleaned) {
        return;
    }
    // A Markdown link to another `.md` file is a note citation, not a source
    // file mention: the notes store itself is Markdown. Check this before the
    // generic file-suffix branch, which would otherwise claim `Other.md`.
    let lower = cleaned.to_ascii_lowercase();
    let is_note_path = lower.ends_with(".md") || lower.ends_with(".markdown");
    if !is_note_path {
        if let Some(suffix) = file_suffix(&cleaned) {
            if FILE_SUFFIXES.contains(&suffix.as_str()) {
                let id = file_id(&cleaned);
                nodes.push(
                    Node::new(id.clone(), cleaned, NodeKind::File)
                        .at(source_file.to_string(), line),
                );
                edges.push(Edge::new(
                    page_id,
                    id,
                    Relation::Mentions,
                    Confidence::Extracted,
                ));
                return;
            }
        }
    }
    if let Some(target_slug) = note_target_slug(&cleaned) {
        if target_slug == self_slug {
            return;
        }
        let id = note_id(&target_slug);
        edges.push(Edge::new(
            page_id,
            &id,
            Relation::References,
            Confidence::Extracted,
        ));
        nodes.push(
            Node::new(id.clone(), target_slug.replace('-', " "), NodeKind::Note)
                .at(source_file.to_string(), line)
                .with("placeholder", "true"),
        );
    }
}

fn add_code_span(
    page_id: &str,
    span: &str,
    line: usize,
    source_file: &str,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) -> bool {
    let text = span.trim();
    if text.is_empty()
        || text.len() > MAX_CODE_SPAN_CHARS
        || text.starts_with('$')
        || text.starts_with('-')
        || is_external(text)
    {
        return false;
    }
    // A span may be a short command rather than a single token: `cargo test`.
    // Prose pasted into backticks usually runs longer than this, so cap the word
    // count and require every word to look like a command or identifier.
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty()
        || words.len() > MAX_CODE_SPAN_WORDS
        || words.iter().any(|word| {
            !word.chars().any(|c| c.is_ascii_alphabetic())
                || !word
                    .chars()
                    .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | ':' | '/'))
        })
    {
        return false;
    }
    let unwrapped = text.trim_end_matches("()");
    if let Some(suffix) = file_suffix(unwrapped) {
        if FILE_SUFFIXES.contains(&suffix.as_str()) || unwrapped.contains('/') {
            let id = file_id(unwrapped);
            nodes.push(
                Node::new(id.clone(), unwrapped, NodeKind::File).at(source_file.to_string(), line),
            );
            edges.push(Edge::new(
                page_id,
                id,
                Relation::Mentions,
                Confidence::Extracted,
            ));
            return true;
        }
    }
    if unwrapped.chars().count() < 3 || STOPWORDS.contains(&unwrapped.to_ascii_lowercase().as_str())
    {
        return false;
    }
    let id = symbol_id(unwrapped);
    nodes
        .push(Node::new(id.clone(), unwrapped, NodeKind::Symbol).at(source_file.to_string(), line));
    edges.push(Edge::new(
        page_id,
        id,
        Relation::Mentions,
        Confidence::Inferred,
    ));
    true
}

/// Turn a link target into the slug of the note it names, honouring Obsidian's
/// vault-global rule: `[[folder/Note]]` and `[[Note]]` both resolve by basename.
fn note_target_slug(target: &str) -> Option<String> {
    let cleaned = clean_link_target(target);
    if cleaned.is_empty() || is_external(&cleaned) {
        return None;
    }
    let without_extension = cleaned
        .strip_suffix(".md")
        .or_else(|| cleaned.strip_suffix(".markdown"))
        .unwrap_or(&cleaned);
    let basename = without_extension
        .rsplit('/')
        .next()
        .unwrap_or(without_extension);
    let slug = slug(basename);
    if slug.is_empty() {
        None
    } else {
        Some(slug)
    }
}

fn clean_link_target(target: &str) -> String {
    let mut text = target.trim();
    if text.starts_with('<') && text.ends_with('>') && text.len() >= 2 {
        text = &text[1..text.len() - 1];
    }
    if let Some((before, _)) = text.split_once('#') {
        text = before;
    }
    if let Some((before, _)) = text.split_once('?') {
        text = before;
    }
    text.trim().to_string()
}

fn is_external(target: &str) -> bool {
    let lower = target.to_ascii_lowercase();
    lower.contains("://")
        || lower.starts_with("mailto:")
        || lower.starts_with("tel:")
        || lower.starts_with("data:")
        || lower.starts_with("//")
}

fn file_suffix(target: &str) -> Option<String> {
    let name = target.rsplit('/').next().unwrap_or(target);
    let index = name.rfind('.')?;
    if index == 0 || index + 1 >= name.len() {
        return None;
    }
    Some(name[index..].to_ascii_lowercase())
}

/// ATX heading: up to three spaces, 1-6 `#`, whitespace, text. A closing run of
/// `#` is decoration, not part of the title.
fn atx_heading(line: &str) -> Option<(usize, String)> {
    let trimmed = line.trim_start_matches(' ');
    let indent = line.len() - trimmed.len();
    if indent > 3 {
        return None;
    }
    let hashes = trimmed.chars().take_while(|c| *c == '#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let rest = &trimmed[hashes..];
    if !rest.starts_with(' ') && !rest.starts_with('\t') {
        return None;
    }
    let mut title = rest.trim();
    title = title.trim_end_matches('#').trim_end();
    if title.is_empty() {
        return None;
    }
    Some((hashes, title.to_string()))
}

fn opens_fence(line: &str) -> Option<(char, usize)> {
    let trimmed = line.trim_start();
    let ch = trimmed.chars().next()?;
    if ch != '`' && ch != '~' {
        return None;
    }
    let run = trimmed.chars().take_while(|c| *c == ch).count();
    if run < 3 {
        return None;
    }
    if ch == '`' && trimmed[run..].contains('`') {
        return None;
    }
    Some((ch, run))
}

fn closes_fence(line: &str, ch: char, len: usize) -> bool {
    let trimmed = line.trim();
    let run = trimmed.chars().take_while(|c| *c == ch).count();
    run >= len && trimmed[run..].trim().is_empty()
}

fn wikilinks(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = line;
    while let Some(start) = rest.find("[[") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("]]") else {
            break;
        };
        let inner = &after[..end];
        let target = inner.split(['|', '#']).next().unwrap_or("").trim();
        if !target.is_empty() {
            out.push(target.to_string());
        }
        rest = &after[end + 2..];
    }
    out
}

fn markdown_links(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = line;
    while let Some(open) = rest.find("](") {
        let after = &rest[open + 2..];
        let Some(end) = after.find(')') else {
            break;
        };
        let inside = after[..end].trim();
        let target = inside
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_matches(['<', '>']);
        if !target.is_empty() {
            out.push(target.to_string());
        }
        rest = &after[end + 1..];
    }
    out
}

fn code_spans(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = line.char_indices().peekable();
    while let Some((_, ch)) = chars.next() {
        if ch != '`' {
            continue;
        }
        let start = chars.peek().map(|(i, _)| *i).unwrap_or(line.len());
        let Some(relative_end) = line[start..].find('`') else {
            break;
        };
        let end = start + relative_end;
        if end > start {
            out.push(line[start..end].to_string());
        }
        while chars.peek().is_some_and(|(i, _)| *i <= end) {
            chars.next();
        }
    }
    out
}

fn terms_in(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in line.split(|c: char| !c.is_alphanumeric()) {
        let term = raw.to_ascii_lowercase();
        if term.chars().count() >= MIN_TERM_CHARS
            && !STOPWORDS.contains(&term.as_str())
            && term.chars().any(|c| c.is_ascii_alphabetic())
        {
            out.push(term);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn note(id: &str, content: &str) -> Note {
        Note {
            title: id.replace('-', " "),
            id: id.to_string(),
            content: content.to_string(),
            summary_pointer: String::new(),
            tags: Vec::new(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            parent_id: None,
            part_number: None,
            part_count: None,
            content_hash: None,
        }
    }

    #[test]
    fn headings_nest_by_level() {
        let extraction = extract_note(&note("a", "# Top\n\n## Child\n\n### Deep\n\n## Sibling"));
        let edges: Vec<_> = extraction
            .edges
            .iter()
            .filter(|e| e.relation == Relation::Contains)
            .map(|e| (e.source.clone(), e.target.clone()))
            .collect();
        assert_eq!(edges.len(), 4);
        let child = edges
            .iter()
            .find(|(_, target)| target.contains("child"))
            .expect("child heading");
        assert!(child.0.starts_with("heading:a:top:1"), "{child:?}");
        let deep = edges
            .iter()
            .find(|(_, target)| target.contains("deep"))
            .expect("deep heading");
        assert!(deep.0.contains("child"), "{deep:?}");
    }

    #[test]
    fn fenced_code_is_not_a_heading() {
        let extraction = extract_note(&note("a", "```\n# not a heading\n```\n# real"));
        assert!(extraction.nodes.iter().any(|n| n.label == "real"));
        assert!(!extraction.nodes.iter().any(|n| n.label == "not a heading"));
    }

    #[test]
    fn wikilinks_are_references_and_resolve_basenames() {
        let extraction = extract_note(&note("a", "See [[folder/Other Note|alias]] and [[A]]"));
        let targets: Vec<_> = extraction
            .edges
            .iter()
            .filter(|e| e.relation == Relation::References)
            .map(|e| e.target.clone())
            .collect();
        assert_eq!(targets, vec!["note:other-note".to_string()]);
    }

    #[test]
    fn self_links_are_ignored() {
        let extraction = extract_note(&note("a", "See [[a]]"));
        assert!(!extraction
            .edges
            .iter()
            .any(|e| e.relation == Relation::References));
    }

    #[test]
    fn external_and_anchor_links_are_skipped() {
        let extraction = extract_note(&note(
            "a",
            "[web](https://example.com) [anchor](#section) [local](Other.md)",
        ));
        let targets: Vec<_> = extraction
            .edges
            .iter()
            .filter(|e| e.relation == Relation::References)
            .map(|e| e.target.clone())
            .collect();
        assert_eq!(targets, vec!["note:other".to_string()]);
    }

    #[test]
    fn tags_and_code_spans_become_nodes() {
        let mut n = note("a", "Run `cargo test` then edit `src/lib.rs`.");
        n.tags = vec!["build".into(), "rust".into()];
        let extraction = extract_note(&n);
        assert!(extraction.nodes.iter().any(|x| x.id == "tag:build"));
        assert!(extraction.nodes.iter().any(|x| x.id == "file:src-lib-rs"));
        assert!(extraction.nodes.iter().any(|x| x.id == "symbol:cargo-test"));
    }

    #[test]
    fn parent_id_becomes_part_of_edge() {
        let mut n = note("child", "body");
        n.parent_id = Some("parent".into());
        let extraction = extract_note(&n);
        assert!(extraction
            .edges
            .iter()
            .any(|e| e.relation == Relation::PartOf && e.target == "note:parent"));
    }

    #[test]
    fn terms_are_lowercase_and_deduplicated() {
        let extraction = extract_note(&note("a", "Compaction Compaction memory"));
        assert_eq!(
            extraction.terms,
            vec!["compaction".to_string(), "memory".to_string()]
        );
    }
}
