//! Diagram sources and the SVG the plane lays out for them (ADP-006, DOMAIN.md §10.4 C).
//!
//! A diagram is a typed IR (`application/vnd.quansio.diagram+json`): nodes, edges and a kind. The
//! plane lays it out deterministically and hands back SVG. The authority accepts the SVG only if
//! it is safe to show and says what the source says: one element per node holding the node's label
//! and one per edge, so a picture cannot quietly omit or invent a step.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use super::html;
use super::quality::Check;
use super::xml::{self, Element, XmlLimits};

const RULE_SAFE: &str = "output.diagram.svg.safe";
const RULE_NODES: &str = "output.diagram.svg.nodes";
const RULE_EDGES: &str = "output.diagram.svg.edges";

/// One node of a diagram.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Node {
    /// `[A-Za-z][A-Za-z0-9_-]{0,31}`, unique.
    pub id: String,
    /// What it says.
    pub label: String,
    /// How it is drawn (`box`, `round`, `diamond`, `actor`).
    #[serde(default)]
    pub shape: Option<String>,
}

/// One edge.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Edge {
    /// The node it leaves.
    pub from: String,
    /// The node it reaches.
    pub to: String,
    /// What it says.
    #[serde(default)]
    pub label: Option<String>,
}

/// A diagram source.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagramIr {
    /// Always `v1`.
    pub schema_version: String,
    /// `flow`, `sequence` or `tree`.
    pub kind: String,
    /// The diagram's title.
    pub title: String,
    /// `TB` or `LR`.
    #[serde(default)]
    pub direction: Option<String>,
    /// The nodes.
    pub nodes: Vec<Node>,
    /// The edges.
    pub edges: Vec<Edge>,
}

/// Bounds from `config/outputs.yaml`.
#[derive(Debug, Clone, Copy)]
pub struct DiagramLimits {
    /// The most nodes.
    pub max_nodes: usize,
    /// The most edges.
    pub max_edges: usize,
    /// The largest SVG, in bytes.
    pub svg_max_bytes: usize,
}

fn valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && id.len() <= 32
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

impl DiagramIr {
    /// Parse and validate a source.
    ///
    /// # Errors
    /// A sentence naming what is wrong.
    pub fn parse(bytes: &[u8], limits: DiagramLimits) -> Result<Self, String> {
        let ir: Self = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        ir.validate(limits)?;
        Ok(ir)
    }

    fn validate(&self, limits: DiagramLimits) -> Result<(), String> {
        if self.schema_version != "v1" {
            return Err("schema_version must be v1".to_string());
        }
        if !matches!(self.kind.as_str(), "flow" | "sequence" | "tree") {
            return Err("kind must be flow, sequence or tree".to_string());
        }
        if self.title.trim().is_empty() || self.title.chars().count() > 120 {
            return Err("the title must be 1 to 120 characters".to_string());
        }
        if let Some(direction) = &self.direction {
            if !matches!(direction.as_str(), "TB" | "LR") {
                return Err("direction must be TB or LR".to_string());
            }
        }
        if self.nodes.is_empty() || self.nodes.len() > limits.max_nodes {
            return Err(format!("a diagram has 1 to {} nodes", limits.max_nodes));
        }
        if self.edges.len() > limits.max_edges {
            return Err(format!("a diagram has at most {} edges", limits.max_edges));
        }
        let mut ids = BTreeSet::new();
        for node in &self.nodes {
            if !valid_id(&node.id) {
                return Err(format!("{:?} is not a node id", node.id));
            }
            if !ids.insert(node.id.as_str()) {
                return Err(format!("node {} appears twice", node.id));
            }
            if node.label.trim().is_empty() || node.label.chars().count() > 120 {
                return Err(format!(
                    "node {} needs a label of 1 to 120 characters",
                    node.id
                ));
            }
            if let Some(shape) = &node.shape {
                if !matches!(shape.as_str(), "box" | "round" | "diamond" | "actor") {
                    return Err(format!("node {} has an unknown shape", node.id));
                }
            }
        }
        for edge in &self.edges {
            if !ids.contains(edge.from.as_str()) || !ids.contains(edge.to.as_str()) {
                return Err(format!(
                    "the edge {}->{} names a node that does not exist",
                    edge.from, edge.to
                ));
            }
            if edge
                .label
                .as_ref()
                .is_some_and(|label| label.chars().count() > 120)
            {
                return Err("an edge label is at most 120 characters".to_string());
            }
        }
        if self.kind == "tree" {
            let mut parent: BTreeMap<&str, &str> = BTreeMap::new();
            for edge in &self.edges {
                if parent
                    .insert(edge.to.as_str(), edge.from.as_str())
                    .is_some()
                {
                    return Err(format!("in a tree, {} has more than one parent", edge.to));
                }
            }
            for node in &self.nodes {
                let mut at = node.id.as_str();
                for _ in 0..=self.nodes.len() {
                    match parent.get(at) {
                        Some(next) => at = next,
                        None => break,
                    }
                    if at == node.id {
                        return Err(format!("in a tree, {} is its own ancestor", node.id));
                    }
                }
            }
        }
        Ok(())
    }
}

fn normalise(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

const FORBIDDEN: &[&str] = &[
    "script",
    "foreignobject",
    "iframe",
    "object",
    "embed",
    "audio",
    "video",
    "canvas",
    "a",
    "animate",
    "animatetransform",
    "animatemotion",
    "set",
    "handler",
    "listener",
    "metadata",
];

/// Whether an SVG is safe to show, on its own (the check an uploaded SVG image gets).
#[must_use]
pub fn check_safe(svg: &[u8], max_bytes: usize) -> Check {
    if svg.len() > max_bytes {
        return Check::failed(
            RULE_SAFE,
            format!(
                "the SVG is {} bytes, over the {max_bytes}-byte limit",
                svg.len()
            ),
        );
    }
    let document = match xml::parse(svg, XmlLimits::default()) {
        Ok(document) => document,
        Err(error) => return Check::failed(RULE_SAFE, error.to_string()),
    };
    match scan(&document.root) {
        Ok(()) => Check::passed(RULE_SAFE, "well-formed SVG with a viewBox and no script, frame, link, import or external reference"),
        Err(detail) => Check::failed(RULE_SAFE, detail),
    }
}

fn scan(root: &Element) -> Result<(), String> {
    if root.name != "svg" {
        return Err("the root element is not svg".to_string());
    }
    if root.attr("xmlns") != Some("http://www.w3.org/2000/svg") {
        return Err("the root has no SVG namespace".to_string());
    }
    let numbers: Vec<f64> = root
        .attr("viewBox")
        .unwrap_or("")
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse().ok())
        .collect();
    if numbers.len() != 4 || numbers[2] <= 0.0 || numbers[3] <= 0.0 {
        return Err("the root has no valid viewBox".to_string());
    }
    for element in std::iter::once(root).chain(root.descendants()) {
        let tag = element.local_name().to_ascii_lowercase();
        if FORBIDDEN.contains(&tag.as_str()) {
            return Err(format!("<{tag}> is not allowed in a diagram"));
        }
        for (name, value) in &element.attrs {
            let lowered = name.to_ascii_lowercase();
            if lowered.starts_with("on") {
                return Err(format!("{name} is an event handler"));
            }
            if lowered == "href" || lowered.ends_with(":href") {
                let inline_image = tag == "image" && html::is_inline_image(value);
                if !value.starts_with('#') && !inline_image {
                    return Err(format!("{name} points outside the document"));
                }
            }
            if lowered == "style" {
                html::check_css(value).map_err(|refusal| refusal.detail)?;
            }
            let value = value.to_ascii_lowercase();
            if value.contains("javascript:") || value.contains("vbscript:") {
                return Err(format!("{name} carries a script URL"));
            }
        }
        if tag == "style" {
            html::check_css(&element.text()).map_err(|refusal| refusal.detail)?;
        }
    }
    Ok(())
}

/// Check the SVG the plane laid out against its source.
#[must_use]
pub fn verify(svg: &str, ir: &DiagramIr, limits: DiagramLimits) -> Vec<Check> {
    let mut checks = vec![check_safe(svg.as_bytes(), limits.svg_max_bytes)];
    let Ok(document) = xml::parse(svg.as_bytes(), XmlLimits::default()) else {
        checks.push(Check::failed(
            RULE_NODES,
            "not checked: the SVG does not parse",
        ));
        checks.push(Check::failed(
            RULE_EDGES,
            "not checked: the SVG does not parse",
        ));
        return checks;
    };
    let all = document.root.descendants();

    let mut problems = Vec::new();
    let mut found: BTreeMap<&str, usize> = BTreeMap::new();
    for element in &all {
        if let Some(id) = element.attr("data-node") {
            *found.entry(id).or_default() += 1;
            let wanted = ir.nodes.iter().find(|node| node.id == id);
            match wanted {
                None => problems.push(format!(
                    "the SVG draws a node {id} the source does not have"
                )),
                Some(node) => {
                    if !normalise(&element.text()).contains(&normalise(&node.label)) {
                        problems.push(format!("node {id} does not say {:?}", node.label));
                    }
                }
            }
        }
    }
    for node in &ir.nodes {
        match found.get(node.id.as_str()) {
            Some(1) => {}
            Some(count) => problems.push(format!("node {} is drawn {count} times", node.id)),
            None => problems.push(format!("node {} is not drawn", node.id)),
        }
    }
    checks.push(if problems.is_empty() {
        Check::passed(
            RULE_NODES,
            format!("{} nodes, each drawn once with its label", ir.nodes.len()),
        )
    } else {
        Check::failed(
            RULE_NODES,
            problems.into_iter().take(5).collect::<Vec<_>>().join("; "),
        )
    });

    let mut wanted: BTreeMap<String, usize> = BTreeMap::new();
    for edge in &ir.edges {
        *wanted
            .entry(format!("{}->{}", edge.from, edge.to))
            .or_default() += 1;
    }
    let mut drawn: BTreeMap<String, usize> = BTreeMap::new();
    for element in &all {
        if let Some(key) = element.attr("data-edge") {
            *drawn.entry(key.to_string()).or_default() += 1;
        }
    }
    checks.push(if wanted == drawn {
        Check::passed(RULE_EDGES, format!("{} edges, each drawn", ir.edges.len()))
    } else {
        let missing: Vec<&String> = wanted.keys().filter(|key| drawn.get(*key) != wanted.get(*key)).collect();
        let extra: Vec<&String> = drawn.keys().filter(|key| !wanted.contains_key(*key)).collect();
        Check::failed(
            RULE_EDGES,
            format!("edges differ from the source (missing or miscounted: {missing:?}; not in the source: {extra:?})"),
        )
    });
    checks
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: DiagramLimits = DiagramLimits {
        max_nodes: 50,
        max_edges: 100,
        svg_max_bytes: 100_000,
    };

    fn ir() -> DiagramIr {
        DiagramIr::parse(
            br#"{"schema_version":"v1","kind":"flow","title":"Release","nodes":[{"id":"a","label":"Build"},{"id":"b","label":"Test & ship"}],"edges":[{"from":"a","to":"b","label":"green"}]}"#,
            LIMITS,
        )
        .expect("ir")
    }

    fn svg(body: &str) -> String {
        format!("<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 200 100\">{body}</svg>")
    }

    const GOOD: &str = "<g data-node=\"a\"><rect width=\"10\" height=\"5\"/><text>Build</text></g>\
        <g data-node=\"b\"><rect width=\"10\" height=\"5\"/><text>Test &amp; ship</text></g>\
        <path data-edge=\"a-&gt;b\" d=\"M0 0 L1 1\"/>";

    #[test]
    fn a_picture_that_says_what_the_source_says_passes_every_check() {
        let checks = verify(&svg(GOOD), &ir(), LIMITS);
        assert_eq!(checks.len(), 3);
        assert!(checks.iter().all(|c| c.ok), "{checks:#?}");
    }

    #[test]
    fn a_missing_a_renamed_and_an_invented_node_are_each_refused() {
        let missing = svg("<g data-node=\"a\"><text>Build</text></g><path data-edge=\"a-&gt;b\"/>");
        assert!(!verify(&missing, &ir(), LIMITS)[1].ok);
        let renamed = svg(&GOOD.replace(">Build<", ">Compile<"));
        assert!(!verify(&renamed, &ir(), LIMITS)[1].ok);
        let invented = svg(&format!(
            "{GOOD}<g data-node=\"c\"><text>Surprise</text></g>"
        ));
        assert!(!verify(&invented, &ir(), LIMITS)[1].ok);
        let twice = svg(&format!("{GOOD}<g data-node=\"a\"><text>Build</text></g>"));
        assert!(!verify(&twice, &ir(), LIMITS)[1].ok);
    }

    #[test]
    fn a_missing_or_invented_edge_is_refused() {
        let none = svg(&GOOD.replace("<path data-edge=\"a-&gt;b\" d=\"M0 0 L1 1\"/>", ""));
        assert!(!verify(&none, &ir(), LIMITS)[2].ok);
        let extra = svg(&format!("{GOOD}<path data-edge=\"b-&gt;a\"/>"));
        assert!(!verify(&extra, &ir(), LIMITS)[2].ok);
    }

    #[test]
    fn an_unsafe_svg_fails_the_safety_check_whatever_it_says() {
        for body in [
            "<script>alert(1)</script>",
            "<foreignObject><div/></foreignObject>",
            "<a href=\"https://example.com\"><text>x</text></a>",
            "<image href=\"https://example.com/a.png\"/>",
            "<use href=\"https://example.com/a.svg#x\"/>",
            "<rect onload=\"x()\"/>",
            "<style>@import url(https://example.com/a.css);</style>",
            "<rect style=\"fill:url(https://example.com/x)\"/>",
            "<animate attributeName=\"href\" to=\"javascript:alert(1)\"/>",
            "<rect fill=\"javascript:alert(1)\"/>",
        ] {
            let checks = verify(&svg(&format!("{GOOD}{body}")), &ir(), LIMITS);
            assert!(!checks[0].ok, "{body}: {:?}", checks[0]);
        }
        assert!(
            !check_safe(b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>", 1000).ok,
            "no viewBox"
        );
        assert!(!check_safe(b"<html/>", 1000).ok);
        assert!(!check_safe(b"<!DOCTYPE svg><svg/>", 1000).ok);
        assert!(
            !check_safe(svg(GOOD).as_bytes(), 10).ok,
            "over the size limit"
        );
        assert!(check_safe(svg(GOOD).as_bytes(), 100_000).ok);
    }

    #[test]
    fn a_source_must_be_a_well_formed_graph_of_its_kind() {
        let parse = |json: &str| DiagramIr::parse(json.as_bytes(), LIMITS);
        assert!(parse(r#"{"schema_version":"v1","kind":"flow","title":"t","nodes":[{"id":"a","label":"A"}],"edges":[]}"#).is_ok());
        for bad in [
            r#"{"schema_version":"v2","kind":"flow","title":"t","nodes":[{"id":"a","label":"A"}],"edges":[]}"#,
            r#"{"schema_version":"v1","kind":"mind","title":"t","nodes":[{"id":"a","label":"A"}],"edges":[]}"#,
            r#"{"schema_version":"v1","kind":"flow","title":"t","nodes":[],"edges":[]}"#,
            r#"{"schema_version":"v1","kind":"flow","title":"t","nodes":[{"id":"a","label":"A"},{"id":"a","label":"B"}],"edges":[]}"#,
            r#"{"schema_version":"v1","kind":"flow","title":"t","nodes":[{"id":"1x","label":"A"}],"edges":[]}"#,
            r#"{"schema_version":"v1","kind":"flow","title":"t","nodes":[{"id":"a","label":" "}],"edges":[]}"#,
            r#"{"schema_version":"v1","kind":"flow","title":"t","nodes":[{"id":"a","label":"A"}],"edges":[{"from":"a","to":"zz"}]}"#,
            r#"{"schema_version":"v1","kind":"flow","title":"t","extra":1,"nodes":[{"id":"a","label":"A"}],"edges":[]}"#,
            r#"{"schema_version":"v1","kind":"flow","title":"t","nodes":[{"id":"a","label":"A","shape":"star"}],"edges":[]}"#,
            r#"{"schema_version":"v1","kind":"tree","title":"t","nodes":[{"id":"a","label":"A"},{"id":"b","label":"B"},{"id":"c","label":"C"}],"edges":[{"from":"a","to":"c"},{"from":"b","to":"c"}]}"#,
            r#"{"schema_version":"v1","kind":"tree","title":"t","nodes":[{"id":"a","label":"A"},{"id":"b","label":"B"}],"edges":[{"from":"a","to":"b"},{"from":"b","to":"a"}]}"#,
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        let tight = DiagramLimits {
            max_nodes: 1,
            ..LIMITS
        };
        assert!(DiagramIr::parse(
            br#"{"schema_version":"v1","kind":"flow","title":"t","nodes":[{"id":"a","label":"A"},{"id":"b","label":"B"}],"edges":[]}"#,
            tight
        )
        .is_err());
    }
}
