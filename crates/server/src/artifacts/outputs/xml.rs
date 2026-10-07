//! A small, strict XML reader for verifying generated packages and images (ADP-006).
//!
//! The authority checks a PPTX's parts and an SVG with this reader. It is not a general XML library
//! and refuses what a verifier has no use for: a `DOCTYPE` or an entity declaration (the way an
//! external entity or a billion-laughs document gets in), an entity other than the five predefined
//! ones and numeric references, an undeclared namespace prefix, a duplicate attribute, mismatched
//! tags, more than one root, and nesting or size past a bound. Everything it returns is borrowed
//! from nothing: the tree is owned, so a caller can keep it past the input.

use std::collections::BTreeSet;

/// A parsed element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Element {
    /// The qualified name (`p:sld`).
    pub name: String,
    /// The attributes, in document order.
    pub attrs: Vec<(String, String)>,
    /// The children, in document order.
    pub children: Vec<Node>,
}

/// A child of an element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    /// An element.
    Element(Element),
    /// Character data (entities resolved).
    Text(String),
}

/// A parsed document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    /// The root element.
    pub root: Element,
}

/// Why a document is refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum XmlError {
    /// Not UTF-8.
    #[error("the document is not UTF-8")]
    NotUtf8,
    /// A construct the verifier refuses outright.
    #[error("{0} is not allowed")]
    Forbidden(&'static str),
    /// The text is not well-formed XML.
    #[error("not well-formed at byte {at}: {detail}")]
    Malformed {
        /// Byte offset.
        at: usize,
        /// What was wrong.
        detail: String,
    },
    /// A bound was passed.
    #[error("the document passes the {0} limit")]
    Limit(&'static str),
}

/// Nesting and size bounds.
#[derive(Debug, Clone, Copy)]
pub struct XmlLimits {
    /// The deepest nesting.
    pub max_depth: usize,
    /// The most elements.
    pub max_elements: usize,
}

impl Default for XmlLimits {
    fn default() -> Self {
        Self {
            max_depth: 64,
            max_elements: 200_000,
        }
    }
}

impl Element {
    /// An attribute's value.
    #[must_use]
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// The name without its prefix.
    #[must_use]
    pub fn local_name(&self) -> &str {
        self.name.rsplit(':').next().unwrap_or(&self.name)
    }

    /// Child elements.
    pub fn elements(&self) -> impl Iterator<Item = &Element> {
        self.children.iter().filter_map(|node| match node {
            Node::Element(element) => Some(element),
            Node::Text(_) => None,
        })
    }

    /// Child elements whose local name is `local`.
    pub fn children_named<'a>(&'a self, local: &'a str) -> impl Iterator<Item = &'a Element> {
        self.elements()
            .filter(move |element| element.local_name() == local)
    }

    /// Every descendant element (depth first, document order), not including `self`.
    #[must_use]
    pub fn descendants(&self) -> Vec<&Element> {
        let mut out = Vec::new();
        let mut stack: Vec<&Element> = self.elements().collect::<Vec<_>>();
        stack.reverse();
        while let Some(element) = stack.pop() {
            out.push(element);
            let mut children: Vec<&Element> = element.elements().collect();
            children.reverse();
            stack.extend(children);
        }
        out
    }

    /// The concatenated text of this element and everything inside it.
    #[must_use]
    pub fn text(&self) -> String {
        let mut out = String::new();
        for node in &self.children {
            match node {
                Node::Text(text) => out.push_str(text),
                Node::Element(element) => out.push_str(&element.text()),
            }
        }
        out
    }
}

/// Parse a document.
///
/// # Errors
/// [`XmlError`] for anything the module's rules refuse.
pub fn parse(bytes: &[u8], limits: XmlLimits) -> Result<Document, XmlError> {
    let text = std::str::from_utf8(bytes).map_err(|_| XmlError::NotUtf8)?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    Parser {
        text,
        at: 0,
        limits,
        elements: 0,
    }
    .document()
}

struct Parser<'a> {
    text: &'a str,
    at: usize,
    limits: XmlLimits,
    elements: usize,
}

fn is_name_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || c == ':' || !c.is_ascii()
}

fn is_name_char(c: char) -> bool {
    is_name_start(c) || c.is_ascii_digit() || c == '-' || c == '.'
}

impl Parser<'_> {
    fn rest(&self) -> &str {
        &self.text[self.at..]
    }

    fn malformed(&self, detail: &str) -> XmlError {
        XmlError::Malformed {
            at: self.at,
            detail: detail.to_string(),
        }
    }

    fn skip_space(&mut self) {
        let trimmed = self.rest().trim_start_matches([' ', '\t', '\r', '\n']);
        self.at = self.text.len() - trimmed.len();
    }

    fn document(mut self) -> Result<Document, XmlError> {
        if self.rest().starts_with("<?xml") {
            let end = self
                .rest()
                .find("?>")
                .ok_or_else(|| self.malformed("unterminated declaration"))?;
            self.at += end + 2;
        }
        let mut root = None;
        loop {
            self.skip_space();
            let rest = self.rest();
            if rest.is_empty() {
                break;
            }
            if rest.starts_with("<!--") {
                self.comment()?;
            } else if rest.starts_with("<!DOCTYPE") || rest.starts_with("<!ENTITY") {
                return Err(XmlError::Forbidden("a DOCTYPE or an entity declaration"));
            } else if rest.starts_with("<?") {
                return Err(XmlError::Forbidden("a processing instruction"));
            } else if rest.starts_with('<') {
                if root.is_some() {
                    return Err(self.malformed("more than one root element"));
                }
                let scope = BTreeSet::new();
                root = Some(self.element(&scope, 1)?);
            } else {
                return Err(self.malformed("text outside the root element"));
            }
        }
        root.map(|root| Document { root })
            .ok_or_else(|| self.malformed("no root element"))
    }

    fn comment(&mut self) -> Result<(), XmlError> {
        let end = self
            .rest()
            .find("-->")
            .ok_or_else(|| self.malformed("unterminated comment"))?;
        self.at += end + 3;
        Ok(())
    }

    fn name(&mut self) -> Result<String, XmlError> {
        let rest = self.rest();
        let mut chars = rest.char_indices();
        match chars.next() {
            Some((_, c)) if is_name_start(c) => {}
            _ => return Err(self.malformed("a name was expected")),
        }
        let end = rest
            .char_indices()
            .find(|(_, c)| !is_name_char(*c))
            .map_or(rest.len(), |(index, _)| index);
        let name = rest[..end].to_string();
        self.at += end;
        Ok(name)
    }

    fn element(&mut self, scope: &BTreeSet<String>, depth: usize) -> Result<Element, XmlError> {
        if depth > self.limits.max_depth {
            return Err(XmlError::Limit("nesting"));
        }
        self.elements += 1;
        if self.elements > self.limits.max_elements {
            return Err(XmlError::Limit("element count"));
        }
        self.at += 1; // '<'
        let name = self.name()?;
        let mut attrs: Vec<(String, String)> = Vec::new();
        loop {
            self.skip_space();
            let rest = self.rest();
            if rest.starts_with("/>") {
                self.at += 2;
                let element = Element {
                    name,
                    attrs,
                    children: Vec::new(),
                };
                self.check_prefixes(&element, scope)?;
                return Ok(element);
            }
            if rest.starts_with('>') {
                self.at += 1;
                break;
            }
            if rest.is_empty() {
                return Err(self.malformed("unterminated start tag"));
            }
            let key = self.name()?;
            self.skip_space();
            if !self.rest().starts_with('=') {
                return Err(self.malformed("an attribute needs a value"));
            }
            self.at += 1;
            self.skip_space();
            let quote = self
                .rest()
                .chars()
                .next()
                .filter(|c| *c == '"' || *c == '\'')
                .ok_or_else(|| self.malformed("an attribute value must be quoted"))?;
            self.at += 1;
            let end = self
                .rest()
                .find(quote)
                .ok_or_else(|| self.malformed("unterminated attribute value"))?;
            let raw = &self.text[self.at..self.at + end];
            if raw.contains('<') {
                return Err(self.malformed("'<' in an attribute value"));
            }
            let value = self.decode(raw)?;
            self.at += end + 1;
            if attrs.iter().any(|(existing, _)| *existing == key) {
                return Err(self.malformed("duplicate attribute"));
            }
            attrs.push((key, value));
        }

        let mut inner_scope = scope.clone();
        for (key, _) in &attrs {
            if let Some(prefix) = key.strip_prefix("xmlns:") {
                inner_scope.insert(prefix.to_string());
            }
        }
        let mut element = Element {
            name,
            attrs,
            children: Vec::new(),
        };
        self.check_prefixes(&element, &inner_scope)?;

        loop {
            let rest = self.rest();
            if rest.is_empty() {
                return Err(self.malformed("unterminated element"));
            }
            if rest.starts_with("</") {
                self.at += 2;
                let closing = self.name()?;
                self.skip_space();
                if !self.rest().starts_with('>') {
                    return Err(self.malformed("unterminated end tag"));
                }
                self.at += 1;
                if closing != element.name {
                    return Err(self.malformed(&format!(
                        "end tag {closing} does not close {}",
                        element.name
                    )));
                }
                return Ok(element);
            }
            if rest.starts_with("<!--") {
                self.comment()?;
            } else if rest.starts_with("<![CDATA[") {
                let end = rest
                    .find("]]>")
                    .ok_or_else(|| self.malformed("unterminated CDATA"))?;
                let data = rest["<![CDATA[".len()..end].to_string();
                self.at += end + 3;
                push_text(&mut element, data);
            } else if rest.starts_with("<?") {
                return Err(XmlError::Forbidden("a processing instruction"));
            } else if rest.starts_with("<!") {
                return Err(XmlError::Forbidden("a markup declaration"));
            } else if rest.starts_with('<') {
                let child = self.element(&inner_scope, depth + 1)?;
                element.children.push(Node::Element(child));
            } else {
                let end = rest.find('<').unwrap_or(rest.len());
                let raw = &rest[..end];
                let text = self.decode(raw)?;
                self.at += end;
                push_text(&mut element, text);
            }
        }
    }

    fn check_prefixes(&self, element: &Element, scope: &BTreeSet<String>) -> Result<(), XmlError> {
        let declared =
            |prefix: &str| prefix == "xml" || prefix == "xmlns" || scope.contains(prefix);
        if let Some((prefix, _)) = element.name.split_once(':') {
            if !declared(prefix) {
                return Err(self.malformed(&format!("undeclared namespace prefix {prefix}")));
            }
        }
        for (key, _) in &element.attrs {
            if let Some((prefix, _)) = key.split_once(':') {
                if !declared(prefix) {
                    return Err(self.malformed(&format!("undeclared namespace prefix {prefix}")));
                }
            }
        }
        Ok(())
    }

    fn decode(&self, raw: &str) -> Result<String, XmlError> {
        let mut out = String::with_capacity(raw.len());
        let mut rest = raw;
        while let Some(index) = rest.find('&') {
            out.push_str(&rest[..index]);
            rest = &rest[index + 1..];
            let end = rest
                .find(';')
                .ok_or_else(|| self.malformed("an entity reference has no end"))?;
            let entity = &rest[..end];
            match entity {
                "amp" => out.push('&'),
                "lt" => out.push('<'),
                "gt" => out.push('>'),
                "quot" => out.push('"'),
                "apos" => out.push('\''),
                other => {
                    let code = other
                        .strip_prefix("#x")
                        .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                        .or_else(|| other.strip_prefix('#').and_then(|dec| dec.parse().ok()))
                        .and_then(char::from_u32)
                        .ok_or_else(|| self.malformed("an undeclared entity"))?;
                    out.push(code);
                }
            }
            rest = &rest[end + 1..];
        }
        out.push_str(rest);
        Ok(out)
    }
}

fn push_text(element: &mut Element, text: String) {
    if text.is_empty() {
        return;
    }
    if let Some(Node::Text(last)) = element.children.last_mut() {
        last.push_str(&text);
    } else {
        element.children.push(Node::Text(text));
    }
}

/// Escape character data for output.
#[must_use]
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(text: &str) -> Document {
        parse(text.as_bytes(), XmlLimits::default()).expect(text)
    }

    fn err(text: &str) -> XmlError {
        parse(text.as_bytes(), XmlLimits::default()).expect_err(text)
    }

    #[test]
    fn a_well_formed_document_is_read_into_a_tree() {
        let doc = ok(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
             <p:sld xmlns:p=\"urn:p\" xmlns:a=\"urn:a\"><a:t>Tom &amp; Jerry &#65; &#x42;</a:t><a:r/></p:sld>",
        );
        assert_eq!(doc.root.name, "p:sld");
        assert_eq!(doc.root.local_name(), "sld");
        assert_eq!(doc.root.text(), "Tom & Jerry A B");
        assert_eq!(doc.root.children_named("r").count(), 1);
        assert_eq!(doc.root.descendants().len(), 2);
    }

    #[test]
    fn what_a_verifier_has_no_use_for_is_refused() {
        assert!(matches!(
            err("<!DOCTYPE x [<!ENTITY a \"b\">]><x>&a;</x>"),
            XmlError::Forbidden(_)
        ));
        assert!(matches!(
            err("<x>&unknown;</x>"),
            XmlError::Malformed { .. }
        ));
        assert!(matches!(err("<?pi x?><x/>"), XmlError::Forbidden(_)));
        assert!(matches!(err("<x><?pi x?></x>"), XmlError::Forbidden(_)));
        assert!(matches!(err("<x/><y/>"), XmlError::Malformed { .. }));
        assert!(matches!(err("<x><y></x></y>"), XmlError::Malformed { .. }));
        assert!(matches!(
            err("<x a=\"1\" a=\"2\"/>"),
            XmlError::Malformed { .. }
        ));
        assert!(matches!(err("<x a=1/>"), XmlError::Malformed { .. }));
        assert!(matches!(err("<x a=\"<\"/>"), XmlError::Malformed { .. }));
        assert!(matches!(err("<q:x/>"), XmlError::Malformed { .. }));
        assert!(matches!(err("<x q:a=\"1\"/>"), XmlError::Malformed { .. }));
        assert!(matches!(err("<x>"), XmlError::Malformed { .. }));
        assert!(matches!(err("hello"), XmlError::Malformed { .. }));
        assert!(matches!(err(""), XmlError::Malformed { .. }));
        assert_eq!(
            parse(&[0xff, 0xfe, b'<'], XmlLimits::default()).expect_err("utf8"),
            XmlError::NotUtf8
        );
    }

    #[test]
    fn nesting_and_size_are_bounded() {
        let deep = format!("{}{}", "<a>".repeat(70), "</a>".repeat(70));
        assert_eq!(err(&deep), XmlError::Limit("nesting"));
        let wide = format!("<r>{}</r>", "<a/>".repeat(10));
        assert_eq!(
            parse(
                wide.as_bytes(),
                XmlLimits {
                    max_depth: 64,
                    max_elements: 5
                }
            )
            .expect_err("wide"),
            XmlError::Limit("element count")
        );
    }

    #[test]
    fn a_prefix_declared_on_an_ancestor_is_in_scope_and_one_on_a_sibling_is_not() {
        ok("<r xmlns:a=\"urn:a\"><x><a:y/></x></r>");
        assert!(matches!(
            err("<r><x xmlns:a=\"urn:a\"/><a:y/></r>"),
            XmlError::Malformed { .. }
        ));
    }

    #[test]
    fn escaping_round_trips_through_the_reader() {
        let nasty = "a<b>&\"c\"";
        let doc = ok(&format!("<x t=\"{}\">{}</x>", escape(nasty), escape(nasty)));
        assert_eq!(doc.root.text(), nasty);
        assert_eq!(doc.root.attr("t"), Some(nasty));
    }
}
