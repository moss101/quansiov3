//! Printable documents (ADP-006, DOMAIN.md §10.4 C): the page shell the authority prints a Markdown
//! document's HTML in, and the check that what was printed says what the source says.
//!
//! The plane renders Markdown to an HTML *body*; the authority composes the page (its print
//! stylesheet, its content-security-policy), prints it in the render sandbox, and compares the words
//! the browser rendered with the words of the source. Coverage is a multiset comparison: a word the
//! source says twice must be rendered twice.

use std::collections::BTreeMap;

use super::deck::PAGE_CSP;

const PRINT_CSS: &str = "html,body{margin:0;padding:0;background:#ffffff;color:#1a1a1a}\
body{font-family:Georgia,'Times New Roman',serif;font-size:12pt;line-height:1.5}\
h1{font-size:24pt;margin:0 0 12pt}h2{font-size:18pt;margin:18pt 0 8pt}h3{font-size:14pt;margin:14pt 0 6pt}\
h4,h5,h6{font-size:12pt;margin:12pt 0 6pt}\
h1,h2,h3,h4,h5,h6{font-family:'Helvetica Neue',Helvetica,Arial,sans-serif;color:#14355c;page-break-after:avoid;break-after:avoid}\
p{margin:0 0 10pt;orphans:3;widows:3}ul,ol{margin:0 0 10pt;padding-left:24pt}li{margin:0 0 4pt}\
pre{background:#f4f6f9;padding:8pt;white-space:pre-wrap;word-wrap:break-word;font-size:10pt;page-break-inside:avoid;break-inside:avoid}\
code{font-family:Menlo,Consolas,monospace;font-size:10pt}\
blockquote{margin:0 0 10pt;padding-left:12pt;border-left:3pt solid #1f4e79;color:#333333}\
table{border-collapse:collapse;margin:0 0 10pt;page-break-inside:avoid;break-inside:avoid}\
th,td{border:1px solid #888888;padding:4pt 8pt;text-align:left}img{max-width:100%;height:auto}\
hr{border:0;border-top:1px solid #888888;margin:14pt 0}";

/// The page a document body is printed in.
#[must_use]
pub fn document_shell(body: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta http-equiv=\"Content-Security-Policy\" content=\"{PAGE_CSP}\"><style>{PRINT_CSS}</style></head><body>{body}</body></html>"
    )
}

/// Count the alphanumeric words of some text, lower-cased.
#[must_use]
pub fn words(text: &str) -> BTreeMap<String, usize> {
    let mut out: BTreeMap<String, usize> = BTreeMap::new();
    let mut current = String::new();
    for c in text.chars() {
        if c.is_alphanumeric() {
            current.extend(c.to_lowercase());
        } else if !current.is_empty() {
            *out.entry(std::mem::take(&mut current)).or_default() += 1;
        }
    }
    if !current.is_empty() {
        *out.entry(current).or_default() += 1;
    }
    out
}

/// The words a document's Markdown source says that a printed page shows: fence marker lines, a
/// numbered list's own numbers and inline images (which print as pictures) are left out.
#[must_use]
pub fn markdown_words(source: &str) -> BTreeMap<String, usize> {
    let mut kept = String::new();
    for line in source.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            continue;
        }
        let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
        let line = if digits > 0
            && trimmed[digits..].starts_with(['.', ')'])
            && trimmed[digits + 1..].starts_with([' ', '\t'])
        {
            &trimmed[digits + 1..]
        } else {
            line
        };
        kept.push_str(&strip_inline_images(line));
        kept.push('\n');
    }
    words(&kept)
}

fn strip_inline_images(line: &str) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some(start) = rest.find("![") {
        let after = &rest[start + 2..];
        let Some(close) = after.find("](data:") else {
            break;
        };
        let target = &after[close + 2..];
        let Some(end) = target.find(')') else {
            break;
        };
        out.push_str(&rest[..start]);
        rest = &target[end + 1..];
    }
    out.push_str(rest);
    out
}

/// The share of the source's words (counted with multiplicity) that the rendered text has.
#[must_use]
pub fn coverage(source: &BTreeMap<String, usize>, rendered: &BTreeMap<String, usize>) -> f64 {
    let total: usize = source.values().sum();
    if total == 0 {
        return 1.0;
    }
    let found: usize = source
        .iter()
        .map(|(word, count)| (*count).min(rendered.get(word).copied().unwrap_or(0)))
        .sum();
    found as f64 / total as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_are_alphanumeric_runs_in_lower_case_with_their_counts() {
        let counted = words("Hello, hello WORLD — it's 2026!");
        assert_eq!(counted["hello"], 2);
        assert_eq!(counted["world"], 1);
        assert_eq!(counted["2026"], 1);
        assert_eq!(counted["it"], 1);
        assert_eq!(counted["s"], 1);
        assert!(words("  ... --- ").is_empty());
    }

    #[test]
    fn source_markers_that_a_printed_page_does_not_show_are_not_words() {
        let source = "# Title\n\n1. first\n2) second\n\n```rust\nlet x = y;\n```\n\n![alt text](data:image/png;base64,AAAA)\n\nText after.";
        let counted = markdown_words(source);
        assert!(!counted.contains_key("rust"), "the fence's language");
        assert!(
            !counted.contains_key("alt"),
            "an inline image prints as a picture"
        );
        assert!(!counted.contains_key("aaaa"));
        assert!(!counted.contains_key("1"), "a list's own number");
        assert!(!counted.contains_key("2"));
        for word in ["title", "first", "second", "let", "x", "y", "text", "after"] {
            assert!(counted.contains_key(word), "{word}");
        }
    }

    #[test]
    fn coverage_counts_with_multiplicity_and_is_one_for_an_empty_source() {
        let source = words("the cat the dog");
        assert!((coverage(&source, &words("the cat the dog extra")) - 1.0).abs() < f64::EPSILON);
        assert!((coverage(&source, &words("the cat dog")) - 0.75).abs() < f64::EPSILON);
        assert!((coverage(&source, &words("")) - 0.0).abs() < f64::EPSILON);
        assert!((coverage(&words(""), &words("anything")) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn the_shell_forbids_everything_but_inline_styles_and_images() {
        let page = document_shell("<p>x</p>");
        assert!(page.contains("default-src 'none'") && page.contains("script-src 'none'"));
        assert!(page.contains("<p>x</p>") && page.starts_with("<!doctype html>"));
    }
}
