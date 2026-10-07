//! Structural checks on a printed PDF (ADP-006, DOMAIN.md §10.4 B and C).
//!
//! The authority prints PDFs itself, in the render sandbox, but it still reads what it is about to
//! store: a PDF that does not look like one, that carries active content, or whose page count is
//! not the one the source promises is refused. This is a scan, not a renderer: it reads the
//! header, the trailer marker, the catalog and the page tree the browser's PDF writer emits in the
//! clear. The text a PDF holds is checked against the rendered DOM text of the same render, not
//! by decoding fonts here.

/// What the scan found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdfInfo {
    /// The version in the header (`1.4`).
    pub version: String,
    /// How many pages the page tree says, which equals the page objects found.
    pub pages: usize,
}

/// Names whose presence makes a PDF active or able to reach outside itself.
const ACTIVE: &[&str] = &[
    "/JavaScript",
    "/JS ",
    "/JS(",
    "/JS<",
    "/Launch",
    "/EmbeddedFile",
    "/EmbeddedFiles",
    "/SubmitForm",
    "/ImportData",
    "/GoToR",
    "/GoToE",
    "/OpenAction",
    "/RichMedia",
    "/XFA",
    "/AcroForm",
    "/URI",
];

/// Scan a PDF.
///
/// # Errors
/// A sentence naming the first thing that is wrong.
pub fn inspect(bytes: &[u8]) -> Result<PdfInfo, String> {
    if !bytes.starts_with(b"%PDF-1.") {
        return Err("it does not start with a PDF header".to_string());
    }
    let version = String::from_utf8_lossy(&bytes[5..8.min(bytes.len())]).into_owned();
    let tail = &bytes[bytes.len().saturating_sub(1024)..];
    if !contains(tail, b"%%EOF") {
        return Err("it does not end with %%EOF".to_string());
    }
    let text = String::from_utf8_lossy(bytes);
    for name in ACTIVE {
        if text.contains(name) {
            return Err(format!(
                "it carries {} (active or external content)",
                name.trim()
            ));
        }
    }
    if !text.contains("/Type /Catalog") && !text.contains("/Type/Catalog") {
        return Err("it has no catalog".to_string());
    }
    let counts = declared_page_counts(&text);
    let [declared] = counts.as_slice() else {
        return Err(format!(
            "it has {} page-tree roots; expected exactly one",
            counts.len()
        ));
    };
    let objects = page_objects(&text);
    if objects != *declared {
        return Err(format!(
            "the page tree says {declared} pages and the file holds {objects} page objects"
        ));
    }
    if *declared == 0 {
        return Err("it has no pages".to_string());
    }
    Ok(PdfInfo {
        version,
        pages: *declared,
    })
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// `/Count N` of every `/Type /Pages` node that has no `/Parent`: the roots of the page tree.
fn declared_page_counts(text: &str) -> Vec<usize> {
    let mut counts = Vec::new();
    for object in text.split(" obj").skip(1) {
        let body = object.split("endobj").next().unwrap_or(object);
        let is_pages = body.contains("/Type /Pages") || body.contains("/Type/Pages");
        if !is_pages || body.contains("/Parent") {
            continue;
        }
        if let Some(count) = body
            .split("/Count")
            .nth(1)
            .and_then(|rest| {
                rest.trim_start()
                    .split(|c: char| !c.is_ascii_digit())
                    .next()
            })
            .and_then(|digits| digits.parse().ok())
        {
            counts.push(count);
        }
    }
    counts
}

/// How many `/Type /Page` objects (not `/Pages`) the file holds.
fn page_objects(text: &str) -> usize {
    let mut count = 0;
    for marker in ["/Type /Page", "/Type/Page"] {
        let mut rest = text;
        while let Some(index) = rest.find(marker) {
            let after = &rest[index + marker.len()..];
            if !after.starts_with('s') {
                count += 1;
            }
            rest = after;
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pdf(pages: usize, extra: &str) -> Vec<u8> {
        let kids: String = (0..pages).map(|n| format!("{} 0 R ", n + 3)).collect();
        let mut text = format!(
            "%PDF-1.4\n1 0 obj\n<</Type /Catalog /Pages 2 0 R>>\nendobj\n2 0 obj\n<</Type /Pages /Count {pages} /Kids [{kids}]>>\nendobj\n"
        );
        for n in 0..pages {
            text.push_str(&format!(
                "{} 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 960 540]>>\nendobj\n",
                n + 3
            ));
        }
        text.push_str(extra);
        text.push_str("trailer\n<</Root 1 0 R>>\n%%EOF\n");
        text.into_bytes()
    }

    #[test]
    fn a_well_formed_pdf_reports_its_pages() {
        let info = inspect(&pdf(3, "")).expect("pdf");
        assert_eq!(
            info,
            PdfInfo {
                version: "1.4".to_string(),
                pages: 3
            }
        );
        assert_eq!(inspect(&pdf(1, "")).expect("one").pages, 1);
    }

    #[test]
    fn a_page_count_that_disagrees_with_the_page_objects_is_refused() {
        let mut text = String::from_utf8(pdf(3, "")).expect("text");
        text = text.replace("/Count 3", "/Count 5");
        assert!(inspect(text.as_bytes())
            .expect_err("count")
            .contains("page objects"));
    }

    #[test]
    fn something_that_is_not_a_pdf_or_is_cut_short_is_refused() {
        assert!(inspect(b"<html>").is_err());
        assert!(inspect(b"").is_err());
        let whole = pdf(2, "");
        assert!(inspect(&whole[..whole.len() - 20])
            .expect_err("cut")
            .contains("%%EOF"));
        assert!(inspect(b"%PDF-1.4\n%%EOF").is_err());
    }

    #[test]
    fn active_or_external_content_is_refused() {
        for extra in [
            "9 0 obj\n<</S /JavaScript /JS (app.alert(1))>>\nendobj\n",
            "9 0 obj\n<</S /Launch /F (cmd)>>\nendobj\n",
            "9 0 obj\n<</S /URI /URI (https://example.com)>>\nendobj\n",
            "9 0 obj\n<</Type /EmbeddedFile>>\nendobj\n",
            "9 0 obj\n<</OpenAction 3 0 R>>\nendobj\n",
        ] {
            assert!(inspect(&pdf(1, extra)).is_err(), "{extra}");
        }
    }

    #[test]
    fn a_pages_node_is_not_counted_as_a_page() {
        assert_eq!(
            page_objects("/Type /Pages /Type /Page /Type/Pages /Type/Page"),
            2
        );
    }
}
