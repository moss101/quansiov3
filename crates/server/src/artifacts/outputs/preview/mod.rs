//! The web-dev preview (ADP-006, DOMAIN.md §10.4 F): short-lived scopes that serve one directory of a
//! workspace root over loopback HTTP, with authority re-resolved on every request, and the
//! suggestions a page sends back.
//!
//! This is scope, authority, expiry and staging logic plus a static file server. It is **not** an
//! in-iframe edit runtime (nothing is injected into served pages) and it does not proxy a running
//! dev server; both are outside the contract.

pub mod authority;
pub mod clock;
mod serve;
pub mod service;
pub mod suggest;

pub use authority::{
    AuthorityError, Binding, DbPreviewAuthority, Lapse, PreviewAuthority, Resolved,
};
pub use clock::{Clock, ManualClock, SystemClock};
pub use serve::{content_type, resolve_file, segments_of, FileError};
pub use service::{
    clean_directory, Active, Listed, OpenError, Opened, PreviewService, Renewed, ScopeError,
    StageError,
};
