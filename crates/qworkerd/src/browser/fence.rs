//! Stale-page fencing: the vocabulary of "the page the model actually observed" (ADP-001).
//!
//! A model acts on a description of a page, and the page keeps moving. Every mutating action therefore
//! names what it is acting on in a form that can be checked against the live page, and fails closed
//! when it cannot be:
//!
//! * **document revision** — an opaque token that changes whenever a tab navigates or its frame tree
//!   changes. An action carries the revision the model last saw; a different one is
//!   `BROWSER_STALE_PAGE`.
//! * **snapshot + ref** — an observation returns a `snapshot_id` and an element `ref` for each
//!   interactive element. A ref is bound to its snapshot, its tab and the revision it was taken at, and
//!   is *never silently re-resolved*: if the element behind it is gone or has become a different
//!   element, the action is stale, not retargeted.
//! * **capture + point** — a screenshot returns a `capture_id`; a point `(x, y)` is bound to that
//!   capture and to the scroll position it was taken at. Per-tile digests recorded at capture time let
//!   the driver tell that pixels near the point changed (a warning, not a refusal, because the page
//!   legitimately animates).
//!
//! Snapshots and captures are invalidated by any mutation, so a ref is good for one action. That is
//! the cost of never acting on a page the model has not just read.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::fault::{BrowserFault, FaultCode};

/// An element's semantic fingerprint: what the model was told it was, checked again before acting.
///
/// The backend node id says *which node*; the fingerprint says *which thing*. A node whose role, name,
/// tag or frame origin has changed since the observation is a different thing, even if it is the same
/// node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    /// The ARIA role the accessibility tree gave it.
    pub role: String,
    /// Its accessible name.
    pub name: String,
    /// Its tag.
    pub tag: String,
    /// The origin of the frame it lives in (`null` for an opaque origin).
    pub frame_origin: String,
}

impl Fingerprint {
    /// The first field that differs, or `None` when the two describe the same thing.
    #[must_use]
    pub fn difference(&self, live: &Self) -> Option<&'static str> {
        if self.tag != live.tag {
            Some("tag")
        } else if self.role != live.role {
            Some("role")
        } else if self.name != live.name {
            Some("name")
        } else if self.frame_origin != live.frame_origin {
            Some("frame_origin")
        } else {
            None
        }
    }

    /// The wire form (names are page content; the caller labels them).
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "role": self.role,
            "name": self.name,
            "tag": self.tag,
            "frame_origin": self.frame_origin,
        })
    }
}

/// One interactive element of a snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElementRef {
    /// The opaque reference the model passes back (`e12`).
    pub reference: String,
    /// The CDP backend node id the reference is bound to.
    pub backend_node_id: i64,
    /// The frame the element lives in.
    pub frame_id: String,
    /// What it was when observed.
    pub fingerprint: Fingerprint,
    /// Whether it is a password, one-time-code or card field, and which.
    pub sensitive: Option<&'static str>,
}

/// An observation of a tab: the refs a model may act on, valid for one revision and one action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// `snap_…`.
    pub id: String,
    /// The tab it belongs to.
    pub tab_id: String,
    /// The document revision it was taken at.
    pub revision: String,
    /// Its elements, by reference.
    pub refs: BTreeMap<String, ElementRef>,
}

/// A screenshot the model may point at: the geometry and the pixels it was taken from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capture {
    /// `cap_…`.
    pub id: String,
    /// The tab it belongs to.
    pub tab_id: String,
    /// The document revision it was taken at.
    pub revision: String,
    /// Viewport width in CSS pixels.
    pub width: u32,
    /// Viewport height in CSS pixels.
    pub height: u32,
    /// Horizontal scroll offset it was taken at.
    pub scroll_x: i64,
    /// Vertical scroll offset it was taken at.
    pub scroll_y: i64,
    /// The side of one digest tile, in CSS pixels.
    pub tile_px: u32,
    /// One digest per tile, keyed by `(column, row)`.
    pub tiles: BTreeMap<(u32, u32), String>,
}

impl Capture {
    /// The tile a viewport point falls in.
    #[must_use]
    pub fn tile_of(&self, x: f64, y: f64) -> (u32, u32) {
        let tile = f64::from(self.tile_px.max(1));
        // The caller bounds the point to the viewport, so the casts cannot wrap.
        (
            (x.max(0.0) / tile).floor() as u32,
            (y.max(0.0) / tile).floor() as u32,
        )
    }

    /// The tiles within one tile of the point: the neighbourhood a click could be disturbed from.
    #[must_use]
    pub fn neighbourhood(&self, x: f64, y: f64) -> Vec<(u32, u32)> {
        let (column, row) = self.tile_of(x, y);
        let columns = self.width.div_ceil(self.tile_px.max(1));
        let rows = self.height.div_ceil(self.tile_px.max(1));
        let mut tiles = Vec::new();
        for r in row.saturating_sub(1)..=(row + 1).min(rows.saturating_sub(1)) {
            for c in column.saturating_sub(1)..=(column + 1).min(columns.saturating_sub(1)) {
                tiles.push((c, r));
            }
        }
        tiles
    }

    /// The pixel rectangle of a tile, clipped to the viewport: `(x, y, width, height)`.
    #[must_use]
    pub fn tile_rect(&self, column: u32, row: u32) -> (u32, u32, u32, u32) {
        let x = column * self.tile_px;
        let y = row * self.tile_px;
        (
            x,
            y,
            self.tile_px.min(self.width.saturating_sub(x)),
            self.tile_px.min(self.height.saturating_sub(y)),
        )
    }
}

/// What a mutating action acts on.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    /// An element reference from a snapshot.
    Ref {
        /// The snapshot.
        snapshot_id: String,
        /// The reference within it.
        reference: String,
    },
    /// A point on a screenshot.
    Point {
        /// The capture.
        capture_id: String,
        /// Viewport x, in CSS pixels.
        x: f64,
        /// Viewport y, in CSS pixels.
        y: f64,
    },
}

impl Target {
    /// Read the target out of a tool's arguments.
    ///
    /// Exactly one form must be given: `{snapshot_id, ref}` or `{capture_id, x, y}`. Mixing them, or
    /// giving half of one, is refused rather than guessed at.
    ///
    /// # Errors
    /// Returns [`FaultCode::InvalidRequest`] when the arguments name no target, a partial target or
    /// both forms.
    pub fn from_args(args: &Value) -> Result<Self, BrowserFault> {
        let invalid = |why: &str| BrowserFault::new(FaultCode::InvalidRequest, why.to_string());
        let snapshot_id = args.get("snapshot_id").and_then(Value::as_str);
        let reference = args.get("ref").and_then(Value::as_str);
        let capture_id = args.get("capture_id").and_then(Value::as_str);
        let x = args.get("x").and_then(Value::as_f64);
        let y = args.get("y").and_then(Value::as_f64);
        let ref_form = snapshot_id.is_some() || reference.is_some();
        let point_form = capture_id.is_some() || x.is_some() || y.is_some();
        match (ref_form, point_form) {
            (true, true) => Err(invalid(
                "name the target as {snapshot_id, ref} or as {capture_id, x, y}, not both",
            )),
            (false, false) => Err(invalid(
                "a mutating browser action needs a target: {snapshot_id, ref} or {capture_id, x, y}",
            )),
            (true, false) => match (snapshot_id, reference) {
                (Some(snapshot_id), Some(reference)) => Ok(Self::Ref {
                    snapshot_id: snapshot_id.to_string(),
                    reference: reference.to_string(),
                }),
                _ => Err(invalid("an element target needs both snapshot_id and ref")),
            },
            (false, true) => match (capture_id, x, y) {
                (Some(capture_id), Some(x), Some(y)) if x.is_finite() && y.is_finite() => {
                    Ok(Self::Point {
                        capture_id: capture_id.to_string(),
                        x,
                        y,
                    })
                }
                _ => Err(invalid("a point target needs capture_id, x and y")),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exactly_one_target_form_is_accepted() {
        assert!(matches!(
            Target::from_args(&json!({"snapshot_id": "s", "ref": "e1"})),
            Ok(Target::Ref { .. })
        ));
        assert!(matches!(
            Target::from_args(&json!({"capture_id": "c", "x": 3, "y": 4.5})),
            Ok(Target::Point { .. })
        ));
        for bad in [
            json!({}),
            json!({"snapshot_id": "s"}),
            json!({"ref": "e1"}),
            json!({"capture_id": "c", "x": 1}),
            json!({"snapshot_id": "s", "ref": "e1", "capture_id": "c", "x": 1, "y": 2}),
        ] {
            assert!(Target::from_args(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_fingerprint_names_what_changed() {
        let was = Fingerprint {
            role: "button".into(),
            name: "Save".into(),
            tag: "button".into(),
            frame_origin: "https://a.example".into(),
        };
        assert_eq!(was.difference(&was.clone()), None);
        let mut now = was.clone();
        now.name = "Delete".into();
        assert_eq!(was.difference(&now), Some("name"));
        let mut other_frame = was.clone();
        other_frame.frame_origin = "https://b.example".into();
        assert_eq!(was.difference(&other_frame), Some("frame_origin"));
    }

    #[test]
    fn a_neighbourhood_is_clipped_to_the_viewport() {
        let capture = Capture {
            id: "cap".into(),
            tab_id: "t".into(),
            revision: "r".into(),
            width: 500,
            height: 300,
            scroll_x: 0,
            scroll_y: 0,
            tile_px: 200,
            tiles: BTreeMap::new(),
        };
        assert_eq!(capture.tile_of(0.0, 0.0), (0, 0));
        assert_eq!(capture.neighbourhood(0.0, 0.0).len(), 4);
        assert_eq!(capture.neighbourhood(250.0, 150.0).len(), 6);
        assert_eq!(capture.tile_rect(2, 1), (400, 200, 100, 100));
    }
}
