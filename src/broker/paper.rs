//! A paper as the research lead reads it. ParsEnriched's canonical document is the authority for a
//! paper's text: its blocks in reading order, each with the parser's heading path and section label, and its figures
//! and tables. The lead reads a section whole, so blocks are grouped into sections exactly as the knowledge service
//! groups them (`knowledge-store` `library.rs`, `sections` and `section_key`): a section the knowledge search found is
//! the same section here, by the same key. The document API returns 25 items a page and keeps 2,048 characters of each
//! block; a paragraph that long is counted, so a reader knows the source may hold more of it.
use crate::broker::library::{self, LibraryError, LibraryService};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use uuid::Uuid;

/// Block kinds that are a section's body text, as the knowledge service counts them. Headings start a section;
/// captions (figure and table records carry those), page furniture and bibliography entries are left out.
const BODY_KINDS: &[&str] = &["paragraph", "text", "list", "list_item", "formula", "code"];

/// ParsEnriched's section labels (`parse/sections.py`, `RULES`), the roles a lead may read a paper by.
pub const ROLES: &[&str] = &[
    "abstract",
    "keywords",
    "introduction",
    "methods",
    "results",
    "discussion",
    "conclusion",
    "references",
    "acknowledgements",
    "funding",
    "conflict",
    "supplementary",
    "author_contributions",
    "ethics",
    "data_availability",
];

/// The characters of one block's text the document API returns.
pub const BLOCK_CHARS: usize = 2048;
/// The most blocks one paper's read takes (120 pages of 25); a longer paper is read in part and says so.
pub const MAX_BLOCKS: usize = 3000;
/// The most figures, and the most tables, one paper's read takes.
pub const MAX_PARTS: usize = 200;

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Block {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub ordinal: i64,
    /// The parser's label for the part of the paper this block is in, one of `ROLES` or none.
    #[serde(default)]
    pub section: Option<String>,
    #[serde(default)]
    pub heading_path: Vec<String>,
    #[serde(default)]
    pub page_from: Option<i64>,
    #[serde(default)]
    pub page_to: Option<i64>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Figure {
    #[serde(default)]
    pub ordinal: i64,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub caption: Option<String>,
    /// A description of the image made by a model during parsing: derived, never the figure's own text.
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub page: Option<i64>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Table {
    #[serde(default)]
    pub ordinal: i64,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub caption: Option<String>,
    #[serde(default)]
    pub page: Option<i64>,
}

/// One section: the body blocks under one heading path, in order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Section {
    /// The knowledge service's address for this section within its paper.
    pub key: String,
    pub path: Vec<String>,
    /// The parser's label of the section's first labeled block, when it is one of `ROLES`.
    pub role: Option<String>,
    pub texts: Vec<String>,
    /// Paragraphs the document API returned at its `BLOCK_CHARS` limit, which may continue in the source.
    pub cut: usize,
    pub page_from: Option<i64>,
    pub page_to: Option<i64>,
}

impl Section {
    /// The heading path as one line, `Opening` for text before the first heading.
    pub fn heading(&self) -> String {
        if self.path.is_empty() {
            "Opening".into()
        } else {
            self.path.join(" › ")
        }
    }

    pub fn chars(&self) -> usize {
        self.texts.iter().map(|t| t.chars().count()).sum()
    }

    pub fn pages(&self) -> Option<String> {
        pages(self.page_from, self.page_to)
    }
}

/// A paper's parse: its catalog fields, sections, figures and tables.
#[derive(Clone, Debug, Default)]
pub struct Paper {
    pub title: Option<String>,
    pub year: Option<i32>,
    pub journal: Option<String>,
    pub authors: Vec<String>,
    pub abstract_text: Option<String>,
    pub sections: Vec<Section>,
    pub figures: Vec<Figure>,
    pub tables: Vec<Table>,
    pub figures_total: usize,
    pub tables_total: usize,
    /// The blocks read and the document's block count; fewer read means the paper was read in part.
    pub blocks_read: usize,
    pub blocks_total: usize,
}

impl Paper {
    pub fn complete(&self) -> bool {
        self.blocks_read >= self.blocks_total
            && self.figures.len() >= self.figures_total
            && self.tables.len() >= self.tables_total
    }
}

/// A section's address within its paper, as the knowledge service writes it: the first 16 hex digits of the SHA-256 of
/// its trimmed headings, each followed by 0x1f, and which occurrence of that heading path it is.
pub fn section_key(path: &[String], occurrence: usize) -> String {
    let mut hash = Sha256::new();
    for part in path {
        hash.update(part.trim().as_bytes());
        hash.update([0x1f]);
    }
    let digest = hex::encode(hash.finalize());
    format!("{}-{occurrence}", &digest[..16])
}

/// Whether `key` has the shape `section_key` writes.
pub fn section_key_valid(key: &str) -> bool {
    let Some((digest, occurrence)) = key.split_once('-') else {
        return false;
    };
    digest.len() == 16
        && digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && !occurrence.is_empty()
        && occurrence.len() <= 4
        && occurrence.bytes().all(|b| b.is_ascii_digit())
        && !occurrence.starts_with('0')
}

/// Body blocks grouped into sections by heading path, in ordinal order, as the knowledge service groups them: a heading
/// sets the current path (its own path, or its text alone), a body block without a path of its own takes the current
/// one, and consecutive blocks with one path form one section. The same path later again is its next occurrence.
pub fn sections(blocks: &[Block]) -> Vec<Section> {
    let mut ordered: Vec<&Block> = blocks.iter().collect();
    ordered.sort_by_key(|b| b.ordinal);
    let mut out: Vec<Section> = Vec::new();
    let mut path: Vec<String> = Vec::new();
    for block in ordered {
        if matches!(block.kind.as_str(), "heading" | "section_header") {
            let heading = block.text.trim();
            if !heading.is_empty() {
                path = if block.heading_path.is_empty() {
                    vec![heading.to_owned()]
                } else {
                    block.heading_path.clone()
                };
            }
            continue;
        }
        if !BODY_KINDS.contains(&block.kind.as_str()) || block.text.trim().is_empty() {
            continue;
        }
        let here = if block.heading_path.is_empty() {
            path.clone()
        } else {
            block.heading_path.clone()
        };
        let cut = usize::from(block.text.chars().count() >= BLOCK_CHARS);
        let role = block
            .section
            .as_deref()
            .filter(|label| ROLES.contains(label))
            .map(str::to_owned);
        match out.last_mut() {
            Some(last) if last.path == here => {
                last.texts.push(block.text.trim().to_owned());
                last.cut += cut;
                last.page_to = block.page_to.or(block.page_from).or(last.page_to);
                if last.role.is_none() {
                    last.role = role;
                }
            }
            _ => out.push(Section {
                key: String::new(),
                path: here,
                role,
                texts: vec![block.text.trim().to_owned()],
                cut,
                page_from: block.page_from,
                page_to: block.page_to.or(block.page_from),
            }),
        }
    }
    let mut seen: HashMap<Vec<String>, usize> = HashMap::new();
    for section in &mut out {
        let occurrence = seen.entry(section.path.clone()).or_insert(0);
        *occurrence += 1;
        section.key = section_key(&section.path, *occurrence);
    }
    out
}

/// A paper from its document summary and collections.
pub fn assemble(
    summary: &Value,
    blocks: &[Block],
    figures: Vec<Figure>,
    tables: Vec<Table>,
) -> Paper {
    let total = |collection: &str| {
        summary["page"]["collections"][collection]["total"]
            .as_u64()
            .map_or(0, |n| n as usize)
    };
    Paper {
        title: library::bounded_text(&summary["title"], 2048),
        year: library::year(&summary["published_on"]),
        journal: library::bounded_text(&summary["container_title"], 512),
        authors: summary["authors"]
            .as_array()
            .map(|all| {
                all.iter()
                    .filter_map(library::author_name)
                    .take(200)
                    .collect()
            })
            .unwrap_or_default(),
        abstract_text: library::bounded_text(&summary["abstract"], 6000),
        sections: sections(blocks),
        figures_total: total("figures").max(figures.len()),
        tables_total: total("tables").max(tables.len()),
        figures,
        tables,
        blocks_read: blocks.len(),
        blocks_total: total("blocks").max(blocks.len()),
    }
}

/// Reads a paper's parse from the lab's corpus: the document's blocks, figures and tables a page at a time, within
/// `MAX_BLOCKS` and `MAX_PARTS`. A page the service returns empty ends that collection, and the paper says it was read
/// in part.
pub async fn read(
    service: &LibraryService,
    lab: &str,
    document: Uuid,
) -> Result<Paper, LibraryError> {
    let first = service
        .document_page(lab, document, "blocks,figures,tables", 0)
        .await?;
    let total = |collection: &str| {
        first["page"]["collections"][collection]["total"]
            .as_u64()
            .map_or(0, |n| n as usize)
    };
    let mut blocks: Vec<Block> = items(&first["blocks"]);
    let mut figures: Vec<Figure> = items(&first["figures"]);
    let mut tables: Vec<Table> = items(&first["tables"]);
    while blocks.len() < total("blocks").min(MAX_BLOCKS) {
        let page = service
            .document_page(lab, document, "blocks", blocks.len())
            .await?;
        let more: Vec<Block> = items(&page["blocks"]);
        if more.is_empty() {
            break;
        }
        blocks.extend(more);
    }
    blocks.truncate(MAX_BLOCKS);
    while figures.len() < total("figures").min(MAX_PARTS) {
        let page = service
            .document_page(lab, document, "figures", figures.len())
            .await?;
        let more: Vec<Figure> = items(&page["figures"]);
        if more.is_empty() {
            break;
        }
        figures.extend(more);
    }
    figures.truncate(MAX_PARTS);
    while tables.len() < total("tables").min(MAX_PARTS) {
        let page = service
            .document_page(lab, document, "tables", tables.len())
            .await?;
        let more: Vec<Table> = items(&page["tables"]);
        if more.is_empty() {
            break;
        }
        tables.extend(more);
    }
    tables.truncate(MAX_PARTS);
    Ok(assemble(&first, &blocks, figures, tables))
}

fn items<T: DeserializeOwned>(value: &Value) -> Vec<T> {
    value
        .as_array()
        .map(|all| {
            all.iter()
                .filter_map(|item| serde_json::from_value(item.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Pages as the lead reads them: `3` or `3-5`.
pub fn pages(from: Option<i64>, to: Option<i64>) -> Option<String> {
    match (from, to) {
        (Some(a), Some(b)) if b > a => Some(format!("{a}-{b}")),
        (Some(a), _) => Some(a.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn block(ordinal: i64, kind: &str, text: &str, path: &[&str], section: Option<&str>) -> Block {
        Block {
            kind: kind.into(),
            text: text.into(),
            ordinal,
            section: section.map(str::to_owned),
            heading_path: path.iter().map(|h| (*h).to_owned()).collect(),
            page_from: Some(ordinal / 10 + 1),
            page_to: None,
        }
    }

    #[test]
    fn section_keys_match_the_knowledge_service() {
        // Values from the knowledge service's formula (knowledge-store library.rs section_key), computed independently.
        assert_eq!(section_key(&["Methods".into()], 1), "66214248e9528f7b-1");
        assert_eq!(
            section_key(
                &["2 Materials and Methods".into(), "2.3 Mouse strains".into()],
                2
            ),
            "2bb8b0f09038a87a-2"
        );
        assert_eq!(section_key(&[], 1), "e3b0c44298fc1c14-1");
        assert_eq!(section_key(&[" Results ".into()], 1), "931d2cfa7c905e23-1");
        assert!(section_key_valid("66214248e9528f7b-1"));
        assert!(section_key_valid("2bb8b0f09038a87a-12"));
        for bad in [
            "66214248e9528f7b",
            "66214248E9528F7B-1",
            "66214248e9528f7-1",
            "66214248e9528f7b-0",
            "66214248e9528f7b-01",
            "66214248e9528f7b-12345",
            "66214248e9528f7b-1a",
        ] {
            assert!(!section_key_valid(bad), "{bad}");
        }
    }

    #[test]
    fn blocks_group_into_sections_as_the_knowledge_service_groups_them() {
        let long = "x".repeat(BLOCK_CHARS);
        let blocks = vec![
            block(3, "heading", "Methods", &[], Some("methods")),
            block(0, "paragraph", "Opening words.", &[], None),
            block(4, "paragraph", "First method.", &[], Some("methods")),
            block(5, "caption", "Figure 1. A caption.", &[], Some("methods")),
            block(6, "paragraph", &long, &[], Some("methods")),
            block(7, "heading", "Results", &["Results"], Some("results")),
            block(8, "list_item", "  A result.  ", &[], Some("results")),
            block(9, "paragraph", "   ", &[], Some("results")),
            block(10, "heading", "Methods", &[], Some("methods")),
            block(11, "paragraph", "More method.", &[], None),
            block(
                12,
                "paragraph",
                "Nested.",
                &["Methods", "Mice"],
                Some("methods"),
            ),
            block(
                13,
                "reference",
                "A. Author (2020).",
                &[],
                Some("references"),
            ),
        ];
        let found = sections(&blocks);
        type Shape<'a> = (String, Vec<String>, Option<&'a str>, usize, usize);
        let summary: Vec<Shape> = found
            .iter()
            .map(|s| {
                (
                    s.key.clone(),
                    s.path.clone(),
                    s.role.as_deref(),
                    s.texts.len(),
                    s.cut,
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                (section_key(&[], 1), vec![], None, 1, 0),
                (
                    section_key(&["Methods".into()], 1),
                    vec!["Methods".to_owned()],
                    Some("methods"),
                    2,
                    1
                ),
                (
                    section_key(&["Results".into()], 1),
                    vec!["Results".to_owned()],
                    Some("results"),
                    1,
                    0
                ),
                (
                    section_key(&["Methods".into()], 2),
                    vec!["Methods".to_owned()],
                    None,
                    1,
                    0
                ),
                (
                    section_key(&["Methods".into(), "Mice".into()], 1),
                    vec!["Methods".to_owned(), "Mice".to_owned()],
                    Some("methods"),
                    1,
                    0
                ),
            ]
        );
        assert_eq!(found[2].texts, vec!["A result.".to_owned()]);
        assert_eq!(found[0].heading(), "Opening");
        assert_eq!(found[4].heading(), "Methods › Mice");
        assert_eq!(found[1].chars(), "First method.".len() + BLOCK_CHARS);
        assert_eq!(found[1].pages().as_deref(), Some("1"));
    }

    #[test]
    fn a_paper_counts_what_it_read_against_the_document() {
        let summary = json!({"title": "A study", "published_on": "2021-05-01", "container_title": "Journal",
            "authors": [{"name": "Ada Example"}, {"given": "Bo", "family": "Writer"}], "abstract": "Short.",
            "page": {"collections": {"blocks": {"total": 3}, "figures": {"total": 2}, "tables": {"total": 0}}}});
        let blocks = vec![block(
            0,
            "paragraph",
            "One.",
            &["Introduction"],
            Some("introduction"),
        )];
        let figures = vec![Figure {
            ordinal: 0,
            label: Some("Figure 1".into()),
            ..Figure::default()
        }];
        let paper = assemble(&summary, &blocks, figures, vec![]);
        assert_eq!(paper.title.as_deref(), Some("A study"));
        assert_eq!(paper.year, Some(2021));
        assert_eq!(paper.authors, vec!["Ada Example", "Bo Writer"]);
        assert_eq!((paper.blocks_read, paper.blocks_total), (1, 3));
        assert_eq!((paper.figures.len(), paper.figures_total), (1, 2));
        assert!(!paper.complete());
        assert_eq!(pages(Some(3), Some(5)).as_deref(), Some("3-5"));
        assert_eq!(pages(Some(3), Some(3)).as_deref(), Some("3"));
        assert_eq!(pages(None, Some(3)), None);
    }
}
