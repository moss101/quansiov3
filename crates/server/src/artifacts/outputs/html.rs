//! The markup allowlist for generated pages and documents (ADP-006, DOMAIN.md §10.4 B and C).
//!
//! The plane builds slide fragments and printable document bodies; the authority does not take
//! them on trust. Markup is checked against a closed list of tags and attributes before it goes
//! near the render sandbox, so the sandbox's own refusals (no network, no script) are a second
//! line rather than the only one: a page that reaches for a script, a frame, a form, a remote
//! image, an event handler or a stylesheet import is refused here with the rule that fired.
//!
//! This is a tokenizer, not an HTML parser. It reads tags, attributes and text, refuses comments,
//! `DOCTYPE`, CDATA and processing instructions outright (nothing legitimate in a fragment needs
//! them, and each hides things from a naive scan), refuses anything it cannot tokenize, and checks
//! that start and end tags nest. What it cannot see, the browser refuses.

use std::collections::BTreeMap;

/// The rule a refusal names.
pub const RULE_TAG: &str = "output.html.tag";
/// An attribute outside the allowlist, or an event handler.
pub const RULE_ATTRIBUTE: &str = "output.html.attribute";
/// A URL that is not an inline image or a fragment reference.
pub const RULE_URL: &str = "output.html.url";
/// A stylesheet construct outside the allowlist.
pub const RULE_CSS: &str = "output.html.css";
/// Markup that does not tokenize or nest.
pub const RULE_STRUCTURE: &str = "output.html.structure";

/// A refusal: the rule and what was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// The rule that fired.
    pub rule: &'static str,
    /// What was found.
    pub detail: String,
}

fn refuse<T>(rule: &'static str, detail: impl Into<String>) -> Result<T, Refusal> {
    Err(Refusal {
        rule,
        detail: detail.into(),
    })
}

/// A top-level element of the checked markup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Root {
    /// Its tag, lower case.
    pub tag: String,
    /// Its attributes, names lower case.
    pub attrs: BTreeMap<String, String>,
}

/// What the check found.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Markup {
    /// The elements at the top level of the markup, in order.
    pub roots: Vec<Root>,
    /// How many elements there are in all.
    pub elements: usize,
}

const TAGS: &[&str] = &[
    "section",
    "div",
    "span",
    "p",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "ul",
    "ol",
    "li",
    "strong",
    "em",
    "b",
    "i",
    "u",
    "s",
    "br",
    "hr",
    "blockquote",
    "figure",
    "figcaption",
    "img",
    "code",
    "pre",
    "table",
    "thead",
    "tbody",
    "tr",
    "th",
    "td",
    "small",
    "sup",
    "sub",
    "header",
    "footer",
    "article",
    "dl",
    "dt",
    "dd",
    "del",
    "ins",
    "mark",
    "kbd",
    "abbr",
    "cite",
    // inline vector graphics for charts
    "svg",
    "g",
    "rect",
    "circle",
    "ellipse",
    "line",
    "polyline",
    "polygon",
    "path",
    "text",
    "tspan",
    "title",
    "desc",
];

const VOID: &[&str] = &["br", "hr", "img"];

const ATTRIBUTES: &[&str] = &[
    "class",
    "id",
    "style",
    "lang",
    "dir",
    "title",
    "role",
    "alt",
    "src",
    "width",
    "height",
    "colspan",
    "rowspan",
    "scope",
    "start",
    "data-slide",
    "data-layout",
    "data-role",
    "data-index",
    "aria-label",
    "aria-hidden",
    "aria-labelledby",
    "aria-describedby",
    "viewbox",
    "x",
    "y",
    "x1",
    "x2",
    "y1",
    "y2",
    "cx",
    "cy",
    "r",
    "rx",
    "ry",
    "d",
    "points",
    "fill",
    "stroke",
    "stroke-width",
    "stroke-linecap",
    "stroke-linejoin",
    "stroke-dasharray",
    "fill-opacity",
    "stroke-opacity",
    "opacity",
    "transform",
    "text-anchor",
    "dominant-baseline",
    "font-size",
    "font-weight",
    "font-family",
    "preserveaspectratio",
    "xmlns",
    "href",
];

fn is_tag_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-'
}

fn is_attr_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.')
}

/// Check a piece of markup (a slide fragment or a document body).
///
/// # Errors
/// A [`Refusal`] naming the first rule the markup breaks.
pub fn check_markup(html: &str) -> Result<Markup, Refusal> {
    let mut markup = Markup::default();
    let mut stack: Vec<String> = Vec::new();
    let mut rest = html;
    while let Some(index) = rest.find('<') {
        rest = &rest[index + 1..];
        let Some(first) = rest.chars().next() else {
            return refuse(RULE_STRUCTURE, "the markup ends inside a tag");
        };
        match first {
            '!' | '?' => {
                return refuse(
                    RULE_STRUCTURE,
                    "comments, DOCTYPE, CDATA and processing instructions are not allowed",
                )
            }
            '/' => {
                rest = &rest[1..];
                let end = rest
                    .find('>')
                    .map_or_else(|| refuse(RULE_STRUCTURE, "an end tag is not closed"), Ok)?;
                let name = rest[..end].trim().to_ascii_lowercase();
                rest = &rest[end + 1..];
                match stack.pop() {
                    Some(open) if open == name => {}
                    Some(open) => {
                        return refuse(RULE_STRUCTURE, format!("</{name}> does not close <{open}>"))
                    }
                    None => return refuse(RULE_STRUCTURE, format!("</{name}> closes nothing")),
                }
            }
            c if c.is_ascii_alphabetic() => {
                let name_end = rest.find(|c: char| !is_tag_char(c)).unwrap_or(rest.len());
                let name = rest[..name_end].to_ascii_lowercase();
                rest = &rest[name_end..];
                if !TAGS.contains(&name.as_str()) {
                    return refuse(RULE_TAG, format!("<{name}> is not an allowed element"));
                }
                let (attrs, self_closed, after) = read_attributes(rest, &name)?;
                rest = after;
                markup.elements += 1;
                if stack.is_empty() {
                    markup.roots.push(Root {
                        tag: name.clone(),
                        attrs,
                    });
                }
                let void = VOID.contains(&name.as_str());
                if !void && !self_closed {
                    stack.push(name);
                }
            }
            _ => return refuse(RULE_STRUCTURE, "a '<' that starts no tag"),
        }
    }
    if let Some(open) = stack.last() {
        return refuse(RULE_STRUCTURE, format!("<{open}> is never closed"));
    }
    Ok(markup)
}

fn read_attributes<'a>(
    mut rest: &'a str,
    tag: &str,
) -> Result<(BTreeMap<String, String>, bool, &'a str), Refusal> {
    let mut attrs = BTreeMap::new();
    loop {
        rest = rest.trim_start_matches([' ', '\t', '\r', '\n']);
        if let Some(after) = rest.strip_prefix("/>") {
            return Ok((attrs, true, after));
        }
        if let Some(after) = rest.strip_prefix('>') {
            return Ok((attrs, false, after));
        }
        let name_end = rest.find(|c: char| !is_attr_char(c)).unwrap_or(rest.len());
        if name_end == 0 {
            return refuse(
                RULE_STRUCTURE,
                format!("<{tag}> has something that is not an attribute"),
            );
        }
        let name = rest[..name_end].to_ascii_lowercase();
        rest = rest[name_end..].trim_start_matches([' ', '\t', '\r', '\n']);
        let mut value = String::new();
        if let Some(after) = rest.strip_prefix('=') {
            rest = after.trim_start_matches([' ', '\t', '\r', '\n']);
            if let Some(quote) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') {
                let body = &rest[1..];
                let end = body.find(quote).map_or_else(
                    || refuse(RULE_STRUCTURE, "an attribute value is not closed"),
                    Ok,
                )?;
                value = body[..end].to_string();
                rest = &body[end + 1..];
            } else {
                let end = rest
                    .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
                    .unwrap_or(rest.len());
                value = rest[..end].to_string();
                rest = &rest[end..];
            }
        }
        check_attribute(tag, &name, &value)?;
        if attrs.insert(name.clone(), value).is_some() {
            return refuse(RULE_ATTRIBUTE, format!("<{tag}> repeats {name}"));
        }
    }
}

fn check_attribute(tag: &str, name: &str, value: &str) -> Result<(), Refusal> {
    if name.starts_with("on") {
        return refuse(
            RULE_ATTRIBUTE,
            format!("<{tag} {name}> is an event handler"),
        );
    }
    if !ATTRIBUTES.contains(&name) {
        return refuse(
            RULE_ATTRIBUTE,
            format!("<{tag} {name}> is not an allowed attribute"),
        );
    }
    let lowered = value.to_ascii_lowercase();
    if lowered.contains("javascript:") || lowered.contains("vbscript:") {
        return refuse(RULE_URL, format!("<{tag} {name}> carries a script URL"));
    }
    match name {
        "src" => {
            if tag != "img" || !is_inline_image(value) {
                return refuse(
                    RULE_URL,
                    format!(
                        "<{tag} src> must be an inline png, jpeg, gif or webp data: URI on an img"
                    ),
                );
            }
        }
        "href" => {
            if !value.starts_with('#') {
                return refuse(
                    RULE_URL,
                    format!("<{tag} href> may only point inside the document"),
                );
            }
        }
        "xmlns" => {
            if tag != "svg" || value != "http://www.w3.org/2000/svg" {
                return refuse(RULE_ATTRIBUTE, "xmlns is only allowed on svg, for SVG");
            }
        }
        "style" => check_declarations(value)?,
        _ => {}
    }
    Ok(())
}

/// Whether a value is `data:image/(png|jpeg|gif|webp);base64,<base64>`.
#[must_use]
pub fn is_inline_image(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("data:image/") else {
        return false;
    };
    let Some((kind, body)) = rest.split_once(";base64,") else {
        return false;
    };
    matches!(kind, "png" | "jpeg" | "gif" | "webp")
        && !body.is_empty()
        && body
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
}

fn check_declarations(css: &str) -> Result<(), Refusal> {
    check_css_text(css, false)
}

/// Check a stylesheet.
///
/// # Errors
/// A [`Refusal`] for an `@import`, a font or page rule, a backslash escape, a script construct or a
/// `url()` that is neither an inline image nor a fragment reference.
pub fn check_css(css: &str) -> Result<(), Refusal> {
    check_css_text(css, true)
}

fn check_css_text(css: &str, sheet: bool) -> Result<(), Refusal> {
    if css.contains('\\') {
        return refuse(RULE_CSS, "a backslash escape could hide a keyword");
    }
    if css.contains('<') || css.contains("-->") {
        return refuse(RULE_CSS, "markup inside a stylesheet");
    }
    let lowered = css.to_ascii_lowercase();
    for banned in [
        "@import",
        "@font-face",
        "@page",
        "@namespace",
        "@charset",
        "expression(",
        "javascript:",
        "vbscript:",
        "behavior:",
        "-moz-binding",
        "image-set(",
        "src(",
        "element(",
        "paint(",
    ] {
        if lowered.contains(banned) {
            return refuse(RULE_CSS, format!("{banned} is not allowed"));
        }
    }
    if !sheet && lowered.contains('@') {
        return refuse(RULE_CSS, "an at-rule inside a style attribute");
    }
    let mut rest = lowered.as_str();
    let original = css;
    let mut offset = 0usize;
    while let Some(index) = rest.find("url(") {
        let start = offset + index + 4;
        let body = &original[start..];
        let end = body
            .find(')')
            .map_or_else(|| refuse(RULE_CSS, "a url( is not closed"), Ok)?;
        let target = body[..end].trim().trim_matches(['"', '\'']);
        if !(target.starts_with('#') || is_inline_image(target)) {
            return refuse(
                RULE_URL,
                "url() may only name an inline image or a fragment of the document",
            );
        }
        offset = start + end;
        rest = &lowered[offset..];
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PIXEL: &str =
        "data:image/gif;base64,R0lGODlhAQABAAAAACH5BAEKAAEALAAAAAABAAEAAAICTAEAOw==";

    fn rule(html: &str) -> &'static str {
        check_markup(html).expect_err(html).rule
    }

    #[test]
    fn a_slide_fragment_with_text_lists_an_inline_image_and_a_chart_is_accepted() {
        let html = format!(
            "<section class=\"slide\" data-slide=\"2\" data-layout=\"content\"><h1>Plan</h1><ul><li>One</li><li>Two &amp; three</li></ul>\
             <img src=\"{PIXEL}\" alt=\"a pixel\">\
             <svg viewBox=\"0 0 100 50\" role=\"img\" aria-label=\"bars\" xmlns=\"http://www.w3.org/2000/svg\"><title>bars</title>\
             <rect x=\"0\" y=\"0\" width=\"10\" height=\"20\" fill=\"#336699\"/><text x=\"1\" y=\"2\">7</text></svg>\
             <p style=\"color:#111;background:url(data:image/png;base64,iVBORw0KGgo=)\">x<br>y</p></section>"
        );
        let markup = check_markup(&html).expect("accepted");
        assert_eq!(markup.roots.len(), 1);
        assert_eq!(markup.roots[0].tag, "section");
        assert_eq!(
            markup.roots[0].attrs.get("data-slide").map(String::as_str),
            Some("2")
        );
        assert!(markup.elements >= 10);
    }

    #[test]
    fn script_frames_forms_and_other_active_elements_are_refused() {
        for tag in [
            "script",
            "iframe",
            "object",
            "embed",
            "form",
            "input",
            "button",
            "link",
            "meta",
            "base",
            "style",
            "video",
            "audio",
            "canvas",
            "a",
            "template",
            "noscript",
            "frame",
            "textarea",
            "select",
            "math",
            "foreignobject",
            "use",
            "image",
            "animate",
            "set",
        ] {
            assert_eq!(
                rule(&format!("<section><{tag}></{tag}></section>")),
                RULE_TAG,
                "{tag}"
            );
        }
    }

    #[test]
    fn event_handlers_and_unknown_attributes_are_refused() {
        assert_eq!(rule("<p onclick=\"x()\">a</p>"), RULE_ATTRIBUTE);
        assert_eq!(rule("<p ONMOUSEOVER=x>a</p>"), RULE_ATTRIBUTE);
        assert_eq!(
            rule(&format!("<img src=\"{PIXEL}\" onerror=\"x\">")),
            RULE_ATTRIBUTE
        );
        assert_eq!(rule("<p formaction=\"x\">a</p>"), RULE_ATTRIBUTE);
        assert_eq!(rule("<p srcset=\"x\">a</p>"), RULE_ATTRIBUTE);
        assert_eq!(rule("<p class=\"a\" class=\"b\">a</p>"), RULE_ATTRIBUTE);
        assert_eq!(
            rule("<div xmlns=\"http://www.w3.org/2000/svg\"></div>"),
            RULE_ATTRIBUTE
        );
    }

    #[test]
    fn urls_are_inline_images_or_fragments_and_nothing_else() {
        assert_eq!(
            rule("<img src=\"https://example.com/a.png\" alt=\"\">"),
            RULE_URL
        );
        assert_eq!(rule("<img src=\"//example.com/a.png\" alt=\"\">"), RULE_URL);
        assert_eq!(rule("<img src=\"file:///etc/hosts\" alt=\"\">"), RULE_URL);
        assert_eq!(
            rule("<img src=\"data:image/svg+xml;base64,PHN2Zz4=\" alt=\"\">"),
            RULE_URL
        );
        assert_eq!(
            rule("<img src=\"data:text/html;base64,PHA+\" alt=\"\">"),
            RULE_URL
        );
        assert_eq!(
            rule("<img src=\"data:image/png;base64,\" alt=\"\">"),
            RULE_URL
        );
        assert_eq!(
            rule("<img src=\"data:image/png;base64,AA A\" alt=\"\">"),
            RULE_URL
        );
        assert_eq!(rule("<p title=\"javascript:alert(1)\">a</p>"), RULE_URL);
        assert_eq!(rule("<svg><path href=\"https://x/y\"/></svg>"), RULE_URL);
        assert_eq!(rule("<svg><path href=\"javascript:1\"/></svg>"), RULE_URL);
        assert!(check_markup("<svg><path href=\"#a\"/></svg>").is_ok());
        assert_eq!(
            rule("<div src=\"data:image/png;base64,AAAA\"></div>"),
            RULE_URL
        );
    }

    #[test]
    fn what_hides_things_from_a_naive_scan_is_refused_outright() {
        assert_eq!(rule("<!-- <script> --><p>a</p>"), RULE_STRUCTURE);
        assert_eq!(rule("<!DOCTYPE html><p>a</p>"), RULE_STRUCTURE);
        assert_eq!(rule("<![CDATA[x]]>"), RULE_STRUCTURE);
        assert_eq!(rule("<?xml version=\"1.0\"?><p>a</p>"), RULE_STRUCTURE);
        assert_eq!(rule("<p>a < b</p>"), RULE_STRUCTURE);
        assert_eq!(rule("<p>a"), RULE_STRUCTURE);
        assert_eq!(rule("</p>"), RULE_STRUCTURE);
        assert_eq!(rule("<p><b>a</p></b>"), RULE_STRUCTURE);
        assert_eq!(rule("<p class=\"unterminated>a</p>"), RULE_STRUCTURE);
        assert_eq!(rule("<p"), RULE_STRUCTURE);
        assert_eq!(rule("<p ="), RULE_STRUCTURE);
    }

    #[test]
    fn stylesheets_cannot_import_escape_or_fetch() {
        assert!(
            check_css(".slide{color:#111;background:#fff}@media print{.x{display:none}}").is_ok()
        );
        assert!(check_css(".a{background:url(data:image/png;base64,iVBORw0KGgo=)}").is_ok());
        assert!(check_css(".a{fill:url(#g)}").is_ok());
        for bad in [
            "@import url(https://x/y.css);",
            "@import 'x.css';",
            ".a{background:url(https://example.com/a.png)}",
            ".a{background:url('//example.com/a.png')}",
            ".a{background:url(file:///etc/hosts)}",
            ".a{background:url(data:image/svg+xml;base64,PHN2Zz4=)}",
            ".a{b:u\\72l(x)}",
            "@font-face{font-family:x;src:url(data:font/woff;base64,AA)}",
            "@page{size:10px 10px}",
            ".a{width:expression(alert(1))}",
            ".a{behavior:url(x.htc)}",
            ".a{-moz-binding:url(x)}",
            ".a{background:image-set('a.png' 1x)}",
            ".a{}</style><script>x</script>",
            ".a{b:url(x}",
        ] {
            assert!(check_css(bad).is_err(), "{bad}");
        }
        assert!(check_css(".a{color:red}").is_ok());
    }

    #[test]
    fn a_style_attribute_is_held_to_the_same_rules_and_takes_no_at_rules() {
        assert_eq!(
            rule("<p style=\"background:url(https://x/y)\">a</p>"),
            RULE_URL
        );
        assert_eq!(rule("<p style=\"@import 'x'\">a</p>"), RULE_CSS);
        assert_eq!(rule("<p style=\"width:expression(1)\">a</p>"), RULE_CSS);
        assert!(check_markup("<p style=\"color:#111;margin:0 0 8px\">a</p>").is_ok());
    }
}
