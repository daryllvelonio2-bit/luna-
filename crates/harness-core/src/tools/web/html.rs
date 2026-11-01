//! A deliberately small HTML reader: enough to turn a fetched page into text, and enough to pull
//! links and snippets out of a search-results page. Nothing more.
//!
//! It is hand-written rather than pulled in, and that is a trade stated plainly: this is a tag
//! scanner, not a parser. It does not build a tree, does not implement the error-recovery rules of
//! the HTML spec, and does not know what a `<table>` means beyond "a cell is a word boundary". What
//! it does do is never panic on real-world markup (unclosed tags, stray `<`, attributes with `>`
//! inside a quoted value are the cases it must survive), never allocate per character, and never
//! depend on a crate for a job whose failure mode is "the text reads a little worse".
//!
//! The one structural rule it honours is nesting of *content-dropping* elements: `<script>`,
//! `<style>`, `<noscript>`, `<svg>`, `<iframe>` and `<template>` swallow their contents, and the
//! matching close tag — not the next close tag of any name — is what ends the swallow. Text inside
//! them is source code, not prose, and handing a page's inline JavaScript to a model is both noisy
//! and a way to smuggle instructions past the reader.

/// Elements whose *contents* are not prose and are dropped entirely.
const SKIP_CONTENT: [&str; 6] = ["script", "style", "noscript", "svg", "iframe", "template"];

/// Elements that end a line: text after one of these starts a new block, and a space between two
/// blocks is what keeps adjacent sentences from running together.
const BLOCK: [&str; 21] = [
    "p",
    "div",
    "section",
    "article",
    "aside",
    "header",
    "footer",
    "main",
    "nav",
    "ul",
    "ol",
    "dl",
    "table",
    "tr",
    "blockquote",
    "pre",
    "figure",
    "figcaption",
    "form",
    "hgroup",
    "dialog",
];

/// Table cells become a word boundary rather than a line: a row read as one line is closer to how
/// a person reads a table than one column per line.
const CELL: [&str; 2] = ["td", "th"];

/// `h1`..`h6`, and the deepest heading level the renderer maps to a `#` prefix.
const HEADING_PREFIX_MAX: u8 = 6;

/// A page reduced to what a reader needs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Document {
    /// Contents of `<title>`, when the page had one.
    pub title: Option<String>,
    /// The readable text, block breaks preserved, runs of blank space collapsed.
    pub text: String,
}

/// One element of a given name, with its attributes and its inner text.
///
/// This is what the search backends parse with: a results page is "all the `<a>`" plus "all the
/// `<td>`", and neither caller wants a tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Element {
    /// Lowercased element name.
    pub name: String,
    /// Lowercased attribute name → value, in document order.
    pub attrs: Vec<(String, String)>,
    /// Inner text, entities decoded and whitespace collapsed.
    pub text: String,
}

impl Element {
    /// The value of an attribute, if the element has it.
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// Whether an attribute carries this exact word among space-separated words.
    pub fn has_class(&self, class: &str) -> bool {
        self.attr("class")
            .map(|value| value.split_whitespace().any(|word| word == class))
            .unwrap_or(false)
    }
}

/// One lexical unit of the source.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    /// A start, end or self-closing tag.
    Tag(Tag),
    /// Character data between tags, borrowed from the input.
    Text(String),
    /// `<!-- … -->`, `<!doctype …>`, `<?…?>`: dropped.
    Other,
}

/// A tag, already split into the parts this module cares about.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Tag {
    name: String,
    attrs: Vec<(String, String)>,
    closing: bool,
}

/// Read the page.
pub fn extract(html: &str) -> Document {
    let tokens = tokenize(html);
    let mut out = String::new();
    let mut skipping: Option<String> = None;
    let mut in_title = false;
    let mut title = String::new();
    let mut found_title = None;

    for token in &tokens {
        match token {
            Token::Text(text) => {
                if in_title {
                    title.push_str(text);
                } else if skipping.is_none() {
                    out.push_str(&decode_entities(text));
                }
            }
            Token::Other => {}
            Token::Tag(tag) => {
                if tag.closing {
                    if skipping.as_deref() == Some(tag.name.as_str()) {
                        skipping = None;
                    } else if skipping.is_some() {
                        // Inside a dropped element only its own close tag matters.
                    } else if in_title && tag.name == "title" {
                        in_title = false;
                        let cleaned = collapse_inline(&decode_entities(&title));
                        if !cleaned.is_empty() {
                            found_title = Some(cleaned);
                        }
                    } else if BLOCK.contains(&tag.name.as_str()) {
                        out.push('\n');
                    } else if CELL.contains(&tag.name.as_str()) {
                        out.push(' ');
                    }
                    continue;
                }

                if skipping.is_some() {
                    continue;
                }
                if tag.name == "title" {
                    in_title = true;
                    title.clear();
                } else if SKIP_CONTENT.contains(&tag.name.as_str()) {
                    skipping = Some(tag.name.clone());
                } else if let Some(level) = heading_level(&tag.name) {
                    out.push('\n');
                    for _ in 0..level {
                        out.push('#');
                    }
                    out.push(' ');
                } else if tag.name == "li" {
                    out.push('\n');
                    out.push_str("- ");
                } else if tag.name == "br" || tag.name == "hr" {
                    out.push('\n');
                } else if CELL.contains(&tag.name.as_str()) {
                    out.push(' ');
                } else if BLOCK.contains(&tag.name.as_str()) {
                    out.push('\n');
                }
            }
        }
    }

    Document {
        title: found_title,
        text: normalize(&out),
    }
}

/// The readable text of a page, without the title.
pub fn text_of(html: &str) -> String {
    extract(html).text
}

/// Every element of `name` in document order, with its inner text.
///
/// Nested elements of the same name are one entry: an `<a>` inside an `<a>` is malformed, and the
/// caller wants the outer link, not two fragments of it.
pub fn elements(html: &str, name: &str) -> Vec<Element> {
    let tokens = tokenize(html);
    let mut out: Vec<Element> = Vec::new();
    let mut depth = 0usize;
    let mut attrs: Vec<(String, String)> = Vec::new();
    let mut buffer = String::new();
    let mut skipping: Option<String> = None;

    for token in &tokens {
        match token {
            Token::Text(text) => {
                if depth > 0 && skipping.is_none() {
                    buffer.push_str(text);
                }
            }
            Token::Other => {}
            Token::Tag(tag) => {
                if tag.closing {
                    if skipping.as_deref() == Some(tag.name.as_str()) {
                        skipping = None;
                    } else if skipping.is_some() {
                        // Dropped content: ignore every tag until its own close tag.
                    } else if tag.name == name {
                        depth = depth.saturating_sub(1);
                        if depth == 0 {
                            out.push(Element {
                                name: tag.name.clone(),
                                attrs: std::mem::take(&mut attrs),
                                text: collapse_inline(&decode_entities(&buffer)),
                            });
                            buffer.clear();
                        }
                    } else if depth > 0 && BLOCK.contains(&tag.name.as_str()) {
                        buffer.push(' ');
                    }
                    continue;
                }

                if skipping.is_some() {
                    continue;
                }
                if tag.name == name {
                    if depth == 0 {
                        attrs = tag.attrs.clone();
                        buffer.clear();
                    }
                    depth += 1;
                } else if depth > 0 && SKIP_CONTENT.contains(&tag.name.as_str()) {
                    skipping = Some(tag.name.clone());
                } else if depth > 0 && (BLOCK.contains(&tag.name.as_str()) || tag.name == "br") {
                    buffer.push(' ');
                }
            }
        }
    }
    out
}

/// Split the source into tags and text.
fn tokenize(html: &str) -> Vec<Token> {
    let bytes = html.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0usize;
    let mut text_start = 0usize;

    while index < bytes.len() {
        if bytes[index] != b'<' {
            index += 1;
            continue;
        }
        if index > text_start {
            tokens.push(Token::Text(html[text_start..index].to_string()));
        }
        let rest = &html[index..];
        if rest.starts_with("<!--") {
            let end = rest
                .find("-->")
                .map(|offset| index + offset + 3)
                .unwrap_or(bytes.len());
            tokens.push(Token::Other);
            index = end;
            text_start = index;
            continue;
        }
        if rest.starts_with("<!") || rest.starts_with("<?") {
            let end = rest
                .find('>')
                .map(|offset| index + offset + 1)
                .unwrap_or(bytes.len());
            tokens.push(Token::Other);
            index = end;
            text_start = index;
            continue;
        }
        // A `<` that cannot start a tag is prose: `a < b`, `<3`, `<  spaced`. The HTML spec draws
        // the same line (a tag begins with a letter, `!`, `/` or `?`), and without it a comparison
        // in a sentence would swallow the rest of the page as an attribute salad.
        let plausible = bytes
            .get(index + 1)
            .map(|byte| byte.is_ascii_alphabetic() || matches!(byte, b'/' | b'!' | b'?'))
            .unwrap_or(false);
        if !plausible {
            tokens.push(Token::Text("<".to_string()));
            index += 1;
            text_start = index;
            continue;
        }
        // A `<` with no `>` after it is a comparison in prose, not a tag: emit it as text and keep
        // reading, or the rest of the page would be swallowed.
        let Some(close) = rest.find('>') else {
            index += 1;
            continue;
        };
        match parse_tag(&rest[1..close]) {
            Some(tag) => tokens.push(Token::Tag(tag)),
            None => tokens.push(Token::Text("<".to_string())),
        }
        index += close + 1;
        text_start = index;
    }

    if text_start < bytes.len() {
        tokens.push(Token::Text(html[text_start..].to_string()));
    }
    tokens
}

/// Parse the inside of a tag (`a href="x" target=_blank`), or `None` when it is not a tag.
fn parse_tag(inner: &str) -> Option<Tag> {
    let inner = inner.trim();
    if inner.is_empty() {
        return None;
    }
    let (closing, rest) = match inner.strip_prefix('/') {
        Some(rest) => (true, rest.trim_start()),
        None => (false, inner),
    };
    let name_end = rest
        .find(|c: char| c.is_whitespace() || c == '/')
        .unwrap_or(rest.len());
    let name = rest[..name_end].to_ascii_lowercase();
    // A tag name starts with a letter; `<!doctype>` and `<?xml` are handled before this point, and
    // anything else (`<3`, `<>`) is prose.
    if !name
        .chars()
        .next()
        .map(|c| c.is_ascii_alphabetic())
        .unwrap_or(false)
    {
        return None;
    }
    Some(Tag {
        attrs: parse_attrs(&rest[name_end..]),
        name,
        closing,
    })
}

/// Parse `key="value"`, `key='value'`, `key=value` and bare `key`, all lowercased.
fn parse_attrs(raw: &str) -> Vec<(String, String)> {
    let chars: Vec<char> = raw.chars().collect();
    let mut attrs = Vec::new();
    let mut index = 0usize;

    while index < chars.len() {
        while index < chars.len() && (chars[index].is_whitespace() || chars[index] == '/') {
            index += 1;
        }
        let name_start = index;
        while index < chars.len()
            && !chars[index].is_whitespace()
            && chars[index] != '='
            && chars[index] != '/'
        {
            index += 1;
        }
        if name_start == index {
            break;
        }
        let name: String = chars[name_start..index]
            .iter()
            .collect::<String>()
            .to_ascii_lowercase();
        while index < chars.len() && chars[index].is_whitespace() {
            index += 1;
        }
        let mut value = String::new();
        if index < chars.len() && chars[index] == '=' {
            index += 1;
            while index < chars.len() && chars[index].is_whitespace() {
                index += 1;
            }
            if index < chars.len() && (chars[index] == '"' || chars[index] == '\'') {
                let quote = chars[index];
                index += 1;
                let value_start = index;
                while index < chars.len() && chars[index] != quote {
                    index += 1;
                }
                value = chars[value_start..index].iter().collect();
                if index < chars.len() {
                    index += 1;
                }
            } else {
                let value_start = index;
                while index < chars.len() && !chars[index].is_whitespace() {
                    index += 1;
                }
                value = chars[value_start..index].iter().collect();
            }
        }
        attrs.push((name, value));
    }
    attrs
}

/// The heading level of `h1`..`h6`, if that is what this is.
fn heading_level(name: &str) -> Option<u8> {
    let digits = name.strip_prefix('h')?;
    if digits.len() != 1 {
        return None;
    }
    let level = digits.as_bytes()[0].checked_sub(b'0')?;
    (1..=HEADING_PREFIX_MAX).contains(&level).then_some(level)
}

/// Named entities worth knowing. A page's prose is mostly `&amp;`, `&nbsp;` and typographic quotes;
/// an unrecognised entity is left exactly as written rather than guessed at, so a wrong guess never
/// silently edits someone's code sample.
const NAMED_ENTITIES: [(&str, &str); 26] = [
    ("amp", "&"),
    ("lt", "<"),
    ("gt", ">"),
    ("quot", "\""),
    ("apos", "'"),
    ("nbsp", " "),
    ("ensp", " "),
    ("emsp", " "),
    ("thinsp", " "),
    ("mdash", "—"),
    ("ndash", "–"),
    ("hellip", "…"),
    ("lsquo", "'"),
    ("rsquo", "'"),
    ("ldquo", "\""),
    ("rdquo", "\""),
    ("laquo", "«"),
    ("raquo", "»"),
    ("middot", "·"),
    ("bull", "•"),
    ("copy", "©"),
    ("reg", "®"),
    ("trade", "™"),
    ("deg", "°"),
    ("times", "×"),
    ("divide", "÷"),
];

/// Decode `&amp;`-style entities and numeric references.
pub fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(position) = rest.find('&') {
        out.push_str(&rest[..position]);
        let tail = &rest[position..];
        match entity_at(tail) {
            Some((decoded, consumed)) => {
                out.push(decoded);
                rest = &tail[consumed..];
            }
            None => {
                // Not an entity: emit the `&` and carry on from the character after it.
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The entity starting at `text` (which begins with `&`), and how many bytes it spans.
fn entity_at(text: &str) -> Option<(char, usize)> {
    let end = text.find(';')?;
    // An entity is short; a `&` far from its `;` is a sentence, not a reference.
    if end > 12 {
        return None;
    }
    let name = &text[1..end];
    if let Some(digits) = name.strip_prefix('#') {
        let code = match digits.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => digits.parse::<u32>().ok()?,
        };
        return char::from_u32(code).map(|c| (c, end + 1));
    }
    NAMED_ENTITIES
        .iter()
        .find(|(known, _)| *known == name)
        .and_then(|(_, value)| value.chars().next().map(|c| (c, end + 1)))
}

/// Collapse a run of whitespace to one space and trim the ends: one line of prose.
pub fn collapse_inline(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending_space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(ch);
    }
    out
}

/// Trim every line, collapse the whitespace inside it, and never leave two blank lines in a row.
///
/// A line that is only a marker (`-`, `#`) is dropped with the blanks: an empty list item is a
/// bullet pointing at nothing, which reads as a formatting bug in someone else's document. Runs of
/// blank lines collapse to one because block elements nest — a `<div><div><p>x</p></div></div>` is
/// three block boundaries for one paragraph break, and printing three blank lines makes a short
/// article look like a long gap.
fn normalize(text: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    for raw in text.lines() {
        let collapsed = collapse_inline(raw);
        if collapsed.is_empty() || collapsed.chars().all(|c| c == '-' || c == '#') {
            let open_line = lines.last().map(|line| !line.is_empty()).unwrap_or(false);
            if open_line {
                lines.push(String::new());
            }
            continue;
        }
        lines.push(collapsed);
    }
    while lines.last().map(String::is_empty).unwrap_or(false) {
        lines.pop();
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_title_is_read_and_not_repeated_in_the_body() {
        let document = extract(
            "<html><head><title>Hi &amp; bye</title></head><body><p>Body</p></body></html>",
        );
        assert_eq!(document.title.as_deref(), Some("Hi & bye"));
        assert_eq!(document.text, "Body");
    }

    #[test]
    fn script_style_and_svg_contents_are_dropped_whole() {
        let html = "<body>before<script>if (a < b) { tag('</div>'); }</script>\
                    <style>a { color: red }</style><svg><path d='M0 0'/></svg>after</body>";
        let text = text_of(html);
        assert!(text.contains("before"), "{text}");
        assert!(text.contains("after"), "{text}");
        for dropped in ["if (a", "color: red", "path"] {
            assert!(!text.contains(dropped), "{dropped} survived: {text}");
        }
    }

    #[test]
    fn headings_bullets_and_cells_keep_their_shape() {
        let text = text_of("<h2>Title</h2><ul><li>one</li><li>two</li></ul><table><tr><td>a</td><td>b</td></tr></table>");
        assert!(text.contains("## Title"), "{text}");
        assert!(text.contains("- one"), "{text}");
        assert!(text.contains("- two"), "{text}");
        assert!(text.contains("a b"), "{text}");
    }

    #[test]
    fn entities_are_decoded_and_unknown_ones_are_left_alone() {
        assert_eq!(decode_entities("a&amp;b"), "a&b");
        assert_eq!(decode_entities("&#39;q&#x27;"), "'q'");
        assert_eq!(decode_entities("caf&eacute;"), "caf&eacute;");
        assert_eq!(decode_entities("5 & 6"), "5 & 6");
        assert_eq!(decode_entities("&notanentity;"), "&notanentity;");
        assert_eq!(decode_entities("&#x1F600;"), "\u{1F600}");
    }

    #[test]
    fn malformed_markup_never_panics_and_keeps_its_prose() {
        for html in [
            "<p>unclosed",
            "a < b and c > d",
            "<a href=\"x>y\">link</a>",
            "<",
            "<!-- unterminated comment",
            "<div><span>deep</div></span>",
            "<p>&amp</p>",
            "",
        ] {
            let document = extract(html);
            let _ = document.text;
        }
        assert_eq!(text_of("<p>a < b</p>"), "a < b");
    }

    #[test]
    fn elements_returns_attrs_and_inner_text() {
        let html =
            "<a class='result-link' href='//d.example/x'>One <b>bold</b></a><a href='/y'>Two</a>";
        let links = elements(html, "a");
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].attr("href"), Some("//d.example/x"));
        assert!(links[0].has_class("result-link"));
        assert!(!links[1].has_class("result-link"));
        assert_eq!(links[0].text, "One bold");
        assert_eq!(links[1].text, "Two");
    }

    #[test]
    fn elements_skips_script_content_and_ignores_unrelated_tags() {
        let html = "<td class='s'>keep<script>var x=1</script></td><td>other</td>";
        let cells = elements(html, "td");
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0].text, "keep");
        assert_eq!(cells[1].text, "other");
    }

    #[test]
    fn blank_lines_are_left_for_the_reader_not_stacked() {
        let text = text_of("<p>one</p><div></div><div></div><p>two</p>");
        assert_eq!(text, "one\n\ntwo");
    }

    #[test]
    fn a_list_item_with_nothing_in_it_does_not_become_a_bullet() {
        let text = text_of("<ul><li>real</li><li></li></ul>");
        assert_eq!(text, "- real");
    }
}
