//! Verifying a generated PPTX package (ADP-006, DOMAIN.md §10.4 B).
//!
//! The plane writes a presentation with a minimal OOXML writer; the authority does not take its
//! word for what is in the file. Every package is opened here and checked part by part before a
//! version is stored:
//!
//! * `output.deck.pptx.package`: a ZIP whose parts are stored (not compressed: the writer never
//!   compresses, so a compressed part is not this writer's file), whose CRC-32s hold, whose names
//!   stay inside the package and are not repeated, within the part count and size bounds;
//! * `output.deck.pptx.parts`: every XML part is well-formed (and free of DOCTYPE and entity
//!   tricks), every part has a content type, the required parts exist and every internal
//!   relationship resolves to a part that exists;
//! * `output.deck.pptx.slides`: the presentation lists as many slides as the deck has, each in order
//!   and each a real `p:sld`;
//! * `output.deck.pptx.text`: each slide's text holds the slide's title and every body line;
//! * `output.deck.pptx.notes`: a slide with speaker notes has a notes part holding them;
//! * `output.deck.pptx.external`: no relationship leaves the package and no part carries a macro, an
//!   embedded object or an ActiveX control.

use std::collections::{BTreeMap, BTreeSet};

use super::quality::Check;
use super::xml::{self, Element, XmlLimits};

const RULE_PACKAGE: &str = "output.deck.pptx.package";
const RULE_PARTS: &str = "output.deck.pptx.parts";
const RULE_SLIDES: &str = "output.deck.pptx.slides";
const RULE_TEXT: &str = "output.deck.pptx.text";
const RULE_NOTES: &str = "output.deck.pptx.notes";
const RULE_EXTERNAL: &str = "output.deck.pptx.external";

/// Bounds on a package.
#[derive(Debug, Clone, Copy)]
pub struct PackageLimits {
    /// The most parts.
    pub max_parts: usize,
    /// The largest single part, in bytes.
    pub max_part_bytes: usize,
    /// The largest package, in bytes.
    pub max_bytes: usize,
}

/// A package read from a ZIP: its parts by name.
#[derive(Debug, Clone, Default)]
pub struct Package {
    /// Part name (no leading slash) to its bytes.
    pub parts: BTreeMap<String, Vec<u8>>,
}

/// Why a ZIP is refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ZipError(pub String);

fn zip_error<T>(detail: impl Into<String>) -> Result<T, ZipError> {
    Err(ZipError(detail.into()))
}

fn le16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn le32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// CRC-32 (IEEE), as ZIP uses.
#[must_use]
pub fn crc32(bytes: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut table = [0u32; 256];
        for (index, slot) in table.iter_mut().enumerate() {
            let mut value = index as u32;
            for _ in 0..8 {
                value = if value & 1 == 1 {
                    0xEDB8_8320 ^ (value >> 1)
                } else {
                    value >> 1
                };
            }
            *slot = value;
        }
        table
    });
    let mut crc = 0xFFFF_FFFFu32;
    for byte in bytes {
        crc = table[((crc ^ u32::from(*byte)) & 0xFF) as usize] ^ (crc >> 8);
    }
    !crc
}

/// Read a ZIP whose entries are all stored.
///
/// # Errors
/// [`ZipError`] for anything that is not such a ZIP, or that passes `limits`.
pub fn read_zip(bytes: &[u8], limits: PackageLimits) -> Result<Package, ZipError> {
    if bytes.len() > limits.max_bytes {
        return zip_error(format!(
            "the package is {} bytes, over the limit",
            bytes.len()
        ));
    }
    // End of central directory: the last 22+ bytes; the comment may follow.
    let scan_from = bytes.len().saturating_sub(22 + 65_535);
    let eocd = (scan_from..bytes.len().saturating_sub(21))
        .rev()
        .find(|at| le32(bytes, *at) == Some(0x0605_4b50))
        .map_or_else(|| zip_error("no end-of-central-directory record"), Ok)?;
    let entries = le16(bytes, eocd + 10).unwrap_or(0) as usize;
    let directory_size = le32(bytes, eocd + 12).unwrap_or(0) as usize;
    let directory_at = le32(bytes, eocd + 16).unwrap_or(0) as usize;
    if le16(bytes, eocd + 4) != Some(0) || le16(bytes, eocd + 6) != Some(0) {
        return zip_error("a multi-disk archive");
    }
    if entries == 0xFFFF || directory_size == 0xFFFF_FFFF || directory_at == 0xFFFF_FFFF {
        return zip_error("a ZIP64 archive");
    }
    if entries > limits.max_parts {
        return zip_error(format!(
            "{entries} parts, over the {} limit",
            limits.max_parts
        ));
    }
    if directory_at
        .checked_add(directory_size)
        .is_none_or(|end| end > eocd)
    {
        return zip_error("the central directory is out of range");
    }

    let mut package = Package::default();
    let mut seen = BTreeSet::new();
    let mut cursor = directory_at;
    let mut previous_end = 0usize;
    for _ in 0..entries {
        if le32(bytes, cursor) != Some(0x0201_4b50) {
            return zip_error("a central directory entry is malformed");
        }
        let flags = le16(bytes, cursor + 8).unwrap_or(0);
        let method = le16(bytes, cursor + 10).unwrap_or(99);
        let crc = le32(bytes, cursor + 16).unwrap_or(0);
        let compressed = le32(bytes, cursor + 20).unwrap_or(0) as usize;
        let size = le32(bytes, cursor + 24).unwrap_or(0) as usize;
        let name_len = le16(bytes, cursor + 28).unwrap_or(0) as usize;
        let extra_len = le16(bytes, cursor + 30).unwrap_or(0) as usize;
        let comment_len = le16(bytes, cursor + 32).unwrap_or(0) as usize;
        let local_at = le32(bytes, cursor + 42).unwrap_or(0) as usize;
        let name_bytes = bytes
            .get(cursor + 46..cursor + 46 + name_len)
            .map_or_else(|| zip_error("a part name runs past the directory"), Ok)?;
        let name = std::str::from_utf8(name_bytes).map_or_else(
            |_| zip_error("a part name is not UTF-8"),
            |name| Ok(name.to_string()),
        )?;
        cursor += 46 + name_len + extra_len + comment_len;

        if flags & 1 == 1 {
            return zip_error(format!("{name} is encrypted"));
        }
        if compressed == 0xFFFF_FFFF || size == 0xFFFF_FFFF {
            return zip_error("a ZIP64 entry");
        }
        if name.ends_with('/') {
            continue;
        }
        if name.is_empty()
            || name.starts_with('/')
            || name.contains('\\')
            || name.contains('\0')
            || name
                .split('/')
                .any(|segment| segment == ".." || segment == ".")
        {
            return zip_error(format!("{name:?} is not a name inside the package"));
        }
        if !seen.insert(name.to_ascii_lowercase()) {
            return zip_error(format!("{name} appears twice"));
        }
        if method != 0 {
            return zip_error(format!(
                "{name} is compressed (method {method}); this verifier reads stored parts only"
            ));
        }
        if compressed != size {
            return zip_error(format!("{name} is stored with two different sizes"));
        }
        if size > limits.max_part_bytes {
            return zip_error(format!("{name} is {size} bytes, over the part limit"));
        }
        if le32(bytes, local_at) != Some(0x0403_4b50) {
            return zip_error(format!("{name} has no local header"));
        }
        let local_name_len = le16(bytes, local_at + 26).unwrap_or(0) as usize;
        let local_extra_len = le16(bytes, local_at + 28).unwrap_or(0) as usize;
        if bytes.get(local_at + 30..local_at + 30 + local_name_len) != Some(name.as_bytes()) {
            return zip_error(format!("{name}: the local header names another part"));
        }
        let data_at = local_at + 30 + local_name_len + local_extra_len;
        let data = bytes.get(data_at..data_at + size).map_or_else(
            || zip_error(format!("{name} runs past the end of the file")),
            Ok,
        )?;
        if data_at < previous_end {
            return zip_error(format!("{name} overlaps the part before it"));
        }
        previous_end = data_at + size;
        if crc32(data) != crc {
            return zip_error(format!("{name}: the CRC-32 does not match"));
        }
        package.parts.insert(name, data.to_vec());
    }
    Ok(package)
}

/// A quick structural look at a package, for the format check every stored version gets: a stored
/// ZIP with a content-types part, a presentation and well-formed XML parts. The full verification
/// against a deck is [`verify_pptx`].
///
/// # Errors
/// A sentence naming the first thing wrong.
pub fn inspect_package(bytes: &[u8], limits: PackageLimits) -> Result<usize, String> {
    let package = read_zip(bytes, limits).map_err(|error| error.0)?;
    for required in ["[Content_Types].xml", "_rels/.rels", "ppt/presentation.xml"] {
        if !package.parts.contains_key(required) {
            return Err(format!("{required} is missing"));
        }
    }
    for (name, data) in &package.parts {
        if name.ends_with(".xml") || name.ends_with(".rels") {
            xml::parse(data, XmlLimits::default()).map_err(|error| format!("{name}: {error}"))?;
        }
    }
    Ok(package.parts.len())
}

/// What a deck slide must be found to say in the package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlideExpect {
    /// The slide's title.
    pub title: String,
    /// Each body line, as `deck::body_lines` reads them.
    pub lines: Vec<String>,
    /// The speaker notes, if any.
    pub notes: String,
}

fn fail(rule: &str, detail: impl Into<String>) -> Check {
    Check::failed(rule, detail)
}

fn normalise(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Resolve a relationship target against the part that holds the relationships.
fn resolve(rels_part: &str, target: &str) -> Option<String> {
    let mut segments: Vec<&str> = if let Some(absolute) = target.strip_prefix('/') {
        absolute.split('/').collect()
    } else {
        // `a/_rels/b.xml.rels` holds the relationships of `a/b.xml`: targets resolve against `a/`.
        let mut base: Vec<&str> = rels_part.split('/').collect();
        base.pop();
        if base.last() == Some(&"_rels") {
            base.pop();
        }
        base.extend(target.split('/'));
        base
    };
    let mut out: Vec<&str> = Vec::new();
    for segment in segments.drain(..) {
        match segment {
            "" | "." => {}
            ".." => {
                out.pop()?;
            }
            other => out.push(other),
        }
    }
    Some(out.join("/"))
}

struct Relationship {
    id: String,
    kind: String,
    target: String,
    external: bool,
}

fn relationships(root: &Element) -> Vec<Relationship> {
    root.children_named("Relationship")
        .map(|element| Relationship {
            id: element.attr("Id").unwrap_or_default().to_string(),
            kind: element.attr("Type").unwrap_or_default().to_string(),
            target: element.attr("Target").unwrap_or_default().to_string(),
            external: element
                .attr("TargetMode")
                .is_some_and(|mode| mode.eq_ignore_ascii_case("External")),
        })
        .collect()
}

fn runs_text(root: &Element) -> String {
    // Paragraph by paragraph, so a line is not glued to the next.
    let mut paragraphs = Vec::new();
    for paragraph in std::iter::once(root).chain(root.descendants()) {
        if paragraph.local_name() != "p" {
            continue;
        }
        let text: String = paragraph
            .descendants()
            .into_iter()
            .filter(|element| element.local_name() == "t")
            .map(Element::text)
            .collect();
        if !text.trim().is_empty() {
            paragraphs.push(text);
        }
    }
    paragraphs.join("\n")
}

/// Verify a package against the deck it claims to be.
#[must_use]
pub fn verify_pptx(bytes: &[u8], expect: &[SlideExpect], limits: PackageLimits) -> Vec<Check> {
    let mut checks = Vec::new();
    let package = match read_zip(bytes, limits) {
        Ok(package) => {
            checks.push(Check::passed(
                RULE_PACKAGE,
                format!(
                    "a ZIP of {} stored parts with matching CRC-32s",
                    package.parts.len()
                ),
            ));
            package
        }
        Err(error) => {
            checks.push(fail(RULE_PACKAGE, error.0));
            return checks;
        }
    };

    // --- parts -------------------------------------------------------------------------------
    let mut problems: Vec<String> = Vec::new();
    let mut trees: BTreeMap<&str, Element> = BTreeMap::new();
    for (name, data) in &package.parts {
        if name.ends_with(".xml") || name.ends_with(".rels") {
            match xml::parse(data, XmlLimits::default()) {
                Ok(document) => {
                    trees.insert(name.as_str(), document.root);
                }
                Err(error) => problems.push(format!("{name}: {error}")),
            }
        }
    }
    let content_types = trees.get("[Content_Types].xml");
    match content_types {
        None => problems.push("[Content_Types].xml is missing or unreadable".to_string()),
        Some(root) => {
            let defaults: BTreeSet<String> = root
                .children_named("Default")
                .filter_map(|element| element.attr("Extension"))
                .map(str::to_ascii_lowercase)
                .collect();
            let overrides: BTreeSet<&str> = root
                .children_named("Override")
                .filter_map(|element| element.attr("PartName"))
                .map(|name| name.trim_start_matches('/'))
                .collect();
            for name in package.parts.keys() {
                if name == "[Content_Types].xml" {
                    continue;
                }
                let extension = name
                    .rsplit_once('.')
                    .map_or("", |(_, ext)| ext)
                    .to_ascii_lowercase();
                if !overrides.contains(name.as_str()) && !defaults.contains(&extension) {
                    problems.push(format!("{name} has no content type"));
                }
            }
            for name in &overrides {
                if !package.parts.contains_key(*name) {
                    problems.push(format!(
                        "the content types name {name}, which is not in the package"
                    ));
                }
            }
        }
    }
    for required in [
        "_rels/.rels",
        "ppt/presentation.xml",
        "ppt/_rels/presentation.xml.rels",
    ] {
        if !package.parts.contains_key(required) {
            problems.push(format!("{required} is missing"));
        }
    }
    for (prefix, label) in [
        ("ppt/slideMasters/slideMaster", "a slide master"),
        ("ppt/slideLayouts/slideLayout", "a slide layout"),
        ("ppt/theme/theme", "a theme"),
    ] {
        if !package
            .parts
            .keys()
            .any(|name| name.starts_with(prefix) && name.ends_with(".xml"))
        {
            problems.push(format!("the package has no {label}"));
        }
    }
    let mut external: Vec<String> = Vec::new();
    for (name, root) in &trees {
        if !name.ends_with(".rels") {
            continue;
        }
        for relationship in relationships(root) {
            if relationship.external {
                external.push(format!("{name}: {} leaves the package", relationship.id));
                continue;
            }
            match resolve(name, &relationship.target) {
                Some(target) if package.parts.contains_key(&target) => {}
                Some(target) => problems.push(format!(
                    "{name}: {} points at {target}, which is not in the package",
                    relationship.id
                )),
                None => problems.push(format!(
                    "{name}: {} points outside the package",
                    relationship.id
                )),
            }
        }
    }
    if problems.is_empty() {
        checks.push(Check::passed(
            RULE_PARTS,
            format!(
                "{} parts, every XML part well-formed, every relationship resolves",
                package.parts.len()
            ),
        ));
    } else {
        checks.push(fail(
            RULE_PARTS,
            problems.into_iter().take(5).collect::<Vec<_>>().join("; "),
        ));
    }

    // --- external ----------------------------------------------------------------------------
    for name in package.parts.keys() {
        let lowered = name.to_ascii_lowercase();
        if lowered.ends_with(".bin")
            || lowered.contains("vbaproject")
            || lowered.starts_with("ppt/embeddings/")
            || lowered.starts_with("ppt/activex/")
            || lowered.starts_with("ppt/macros/")
        {
            external.push(format!(
                "{name} is a macro, an embedded object or a control"
            ));
        }
    }
    if external.is_empty() {
        checks.push(Check::passed(
            RULE_EXTERNAL,
            "no external relationship, macro, embedding or control",
        ));
    } else {
        checks.push(fail(
            RULE_EXTERNAL,
            external.into_iter().take(5).collect::<Vec<_>>().join("; "),
        ));
    }

    // --- slides ------------------------------------------------------------------------------
    let slide_parts = slide_parts_in_order(&package, &trees);
    let slides_ok = match &slide_parts {
        Ok(parts) if parts.len() == expect.len() => {
            let mut misnamed = Vec::new();
            for (index, part) in parts.iter().enumerate() {
                let expected = format!("ppt/slides/slide{}.xml", index + 1);
                if *part != expected {
                    misnamed.push(format!("slide {} is {part}, not {expected}", index + 1));
                }
                if !trees
                    .get(part.as_str())
                    .is_some_and(|root| root.local_name() == "sld")
                {
                    misnamed.push(format!("{part} is not a p:sld"));
                }
            }
            if misnamed.is_empty() {
                checks.push(Check::passed(
                    RULE_SLIDES,
                    format!("{} slides in order", parts.len()),
                ));
                true
            } else {
                checks.push(fail(RULE_SLIDES, misnamed.join("; ")));
                false
            }
        }
        Ok(parts) => {
            checks.push(fail(
                RULE_SLIDES,
                format!(
                    "the presentation lists {} slides, the deck has {}",
                    parts.len(),
                    expect.len()
                ),
            ));
            false
        }
        Err(detail) => {
            checks.push(fail(RULE_SLIDES, detail.clone()));
            false
        }
    };

    // --- text and notes ----------------------------------------------------------------------
    if !slides_ok {
        checks.push(fail(
            RULE_TEXT,
            "not checked: the slides do not match the deck",
        ));
        if expect.iter().any(|slide| !slide.notes.trim().is_empty()) {
            checks.push(fail(
                RULE_NOTES,
                "not checked: the slides do not match the deck",
            ));
        }
        return checks;
    }
    let parts = slide_parts.unwrap_or_default();
    let mut missing: Vec<String> = Vec::new();
    let mut notes_missing: Vec<String> = Vec::new();
    for (index, (part, wanted)) in parts.iter().zip(expect).enumerate() {
        let text = trees.get(part.as_str()).map(runs_text).unwrap_or_default();
        let flat = normalise(&text);
        if !flat.contains(&normalise(&wanted.title)) {
            missing.push(format!("slide {} lacks its title", index + 1));
        }
        for line in &wanted.lines {
            if !flat.contains(&normalise(line)) {
                missing.push(format!(
                    "slide {} lacks the line {:?}",
                    index + 1,
                    truncate(line)
                ));
            }
        }
        if !wanted.notes.trim().is_empty() {
            let notes_text = notes_part_for(part, &package, &trees)
                .and_then(|name| trees.get(name.as_str()))
                .map(runs_text)
                .map(|text| normalise(&text));
            match notes_text {
                Some(found) if found.contains(&normalise(&wanted.notes)) => {}
                Some(_) => notes_missing.push(format!(
                    "slide {}: the notes part does not hold the notes",
                    index + 1
                )),
                None => {
                    notes_missing.push(format!("slide {} has notes but no notes part", index + 1))
                }
            }
        }
    }
    if missing.is_empty() {
        checks.push(Check::passed(
            RULE_TEXT,
            "every slide holds its title and every body line",
        ));
    } else {
        checks.push(fail(
            RULE_TEXT,
            missing.into_iter().take(5).collect::<Vec<_>>().join("; "),
        ));
    }
    if expect.iter().any(|slide| !slide.notes.trim().is_empty()) {
        if notes_missing.is_empty() {
            checks.push(Check::passed(
                RULE_NOTES,
                "every slide with speaker notes has them in a notes part",
            ));
        } else {
            checks.push(fail(RULE_NOTES, notes_missing.join("; ")));
        }
    }
    checks
}

fn truncate(text: &str) -> String {
    text.chars().take(40).collect()
}

fn slide_parts_in_order(
    package: &Package,
    trees: &BTreeMap<&str, Element>,
) -> Result<Vec<String>, String> {
    let root_rels = trees
        .get("_rels/.rels")
        .ok_or_else(|| "_rels/.rels is missing".to_string())?;
    let office = relationships(root_rels)
        .into_iter()
        .find(|relationship| relationship.kind.ends_with("/officeDocument"))
        .ok_or_else(|| "the package names no office document".to_string())?;
    let presentation_part = resolve("_rels/.rels", &office.target)
        .filter(|part| package.parts.contains_key(part))
        .ok_or_else(|| "the office document is not in the package".to_string())?;
    let presentation = trees
        .get(presentation_part.as_str())
        .ok_or_else(|| format!("{presentation_part} is unreadable"))?;
    let rels_name = {
        let (directory, file) = presentation_part
            .rsplit_once('/')
            .unwrap_or(("", &presentation_part));
        if directory.is_empty() {
            format!("_rels/{file}.rels")
        } else {
            format!("{directory}/_rels/{file}.rels")
        }
    };
    let rels = trees
        .get(rels_name.as_str())
        .map(relationships)
        .ok_or_else(|| format!("{rels_name} is missing"))?;
    let list = presentation
        .children_named("sldIdLst")
        .next()
        .ok_or_else(|| "the presentation has no slide list".to_string())?;
    let mut parts = Vec::new();
    for entry in list.children_named("sldId") {
        let id = entry
            .attrs
            .iter()
            .find(|(key, _)| key.ends_with(":id") && key != "id")
            .map(|(_, value)| value.as_str())
            .ok_or_else(|| "a slide entry has no relationship id".to_string())?;
        let relationship = rels
            .iter()
            .find(|relationship| relationship.id == id)
            .ok_or_else(|| format!("the slide entry {id} has no relationship"))?;
        if !relationship.kind.ends_with("/slide") {
            return Err(format!("the slide entry {id} is not a slide relationship"));
        }
        let part = resolve(&rels_name, &relationship.target)
            .ok_or_else(|| format!("the slide entry {id} points outside the package"))?;
        if !package.parts.contains_key(&part) {
            return Err(format!(
                "the slide entry {id} points at {part}, which is missing"
            ));
        }
        parts.push(part);
    }
    Ok(parts)
}

fn notes_part_for(
    slide_part: &str,
    package: &Package,
    trees: &BTreeMap<&str, Element>,
) -> Option<String> {
    let (directory, file) = slide_part.rsplit_once('/')?;
    let rels_name = format!("{directory}/_rels/{file}.rels");
    let rels = trees.get(rels_name.as_str()).map(relationships)?;
    let relationship = rels
        .into_iter()
        .find(|relationship| relationship.kind.ends_with("/notesSlide"))?;
    resolve(&rels_name, &relationship.target).filter(|part| package.parts.contains_key(part))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: PackageLimits = PackageLimits {
        max_parts: 64,
        max_part_bytes: 1 << 20,
        max_bytes: 1 << 24,
    };

    /// A stored ZIP written independently of the reader, for the reader's own tests.
    pub(crate) fn zip(parts: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut directory = Vec::new();
        for (name, data) in parts {
            let offset = out.len() as u32;
            let crc = crc32(data);
            out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
            out.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(data);

            directory.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            directory.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            directory.extend_from_slice(&crc.to_le_bytes());
            directory.extend_from_slice(&(data.len() as u32).to_le_bytes());
            directory.extend_from_slice(&(data.len() as u32).to_le_bytes());
            directory.extend_from_slice(&(name.len() as u16).to_le_bytes());
            directory.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            directory.extend_from_slice(&offset.to_le_bytes());
            directory.extend_from_slice(name.as_bytes());
        }
        let directory_at = out.len() as u32;
        out.extend_from_slice(&directory);
        out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        out.extend_from_slice(&[0, 0, 0, 0]);
        out.extend_from_slice(&(parts.len() as u16).to_le_bytes());
        out.extend_from_slice(&(parts.len() as u16).to_le_bytes());
        out.extend_from_slice(&(directory.len() as u32).to_le_bytes());
        out.extend_from_slice(&directory_at.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    #[test]
    fn crc32_matches_the_standard_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn a_stored_zip_is_read_and_a_corrupt_one_is_refused() {
        let file = zip(&[("a.txt", b"hello"), ("dir/b.xml", b"<x/>")]);
        let package = read_zip(&file, LIMITS).expect("read");
        assert_eq!(package.parts["a.txt"], b"hello");
        assert_eq!(package.parts.len(), 2);

        let mut bad_crc = file.clone();
        let at = bad_crc
            .windows(5)
            .position(|w| w == b"hello")
            .expect("data");
        bad_crc[at] = b'j';
        assert!(read_zip(&bad_crc, LIMITS)
            .expect_err("crc")
            .0
            .contains("CRC-32"));

        assert!(read_zip(&file[..file.len() - 10], LIMITS).is_err());
        assert!(read_zip(
            b"not a zip at all, but long enough to scan for a record",
            LIMITS
        )
        .is_err());
        assert!(read_zip(&[], LIMITS).is_err());
    }

    #[test]
    fn names_that_leave_the_package_and_repeated_names_are_refused() {
        for name in ["../x", "/x", "a/../b", "a\\b", "./x"] {
            let file = zip(&[(name, b"x")]);
            assert!(read_zip(&file, LIMITS).is_err(), "{name}");
        }
        let twice = zip(&[("a", b"x"), ("A", b"y")]);
        assert!(read_zip(&twice, LIMITS)
            .expect_err("dup")
            .0
            .contains("twice"));
    }

    #[test]
    fn bounds_on_parts_and_size_hold() {
        let file = zip(&[("a", &[0u8; 100])]);
        let tight = PackageLimits {
            max_part_bytes: 10,
            ..LIMITS
        };
        assert!(read_zip(&file, tight).is_err());
        let few = PackageLimits {
            max_parts: 1,
            ..LIMITS
        };
        let two = zip(&[("a", b"1"), ("b", b"2")]);
        assert!(read_zip(&two, few).is_err());
        let small = PackageLimits {
            max_bytes: 10,
            ..LIMITS
        };
        assert!(read_zip(&file, small).is_err());
    }

    #[test]
    fn a_compressed_entry_is_refused_not_misread() {
        let mut file = zip(&[("a", b"hello")]);
        // Flip the method in the central directory to deflate (8).
        let at = file
            .windows(4)
            .position(|w| w == 0x0201_4b50u32.to_le_bytes())
            .expect("cd");
        file[at + 10] = 8;
        assert!(read_zip(&file, LIMITS)
            .expect_err("method")
            .0
            .contains("compressed"));
    }

    #[test]
    fn targets_resolve_against_the_part_that_holds_the_relationship() {
        assert_eq!(
            resolve("_rels/.rels", "ppt/presentation.xml").as_deref(),
            Some("ppt/presentation.xml")
        );
        assert_eq!(
            resolve("ppt/_rels/presentation.xml.rels", "slides/slide1.xml").as_deref(),
            Some("ppt/slides/slide1.xml")
        );
        assert_eq!(
            resolve(
                "ppt/slides/_rels/slide1.xml.rels",
                "../slideLayouts/slideLayout1.xml"
            )
            .as_deref(),
            Some("ppt/slideLayouts/slideLayout1.xml")
        );
        assert_eq!(
            resolve("ppt/_rels/presentation.xml.rels", "/docProps/app.xml").as_deref(),
            Some("docProps/app.xml")
        );
        assert_eq!(resolve("_rels/.rels", "../../x"), None);
    }
}
