//! The server's end of the worker control channel (EXEC-002, DOMAIN.md §8.1–§8.3).
//!
//! A `customer_private_worker` is outbound-only, so this module **listens** and the worker `qworkerd`
//! **dials in** ([`MachineGateway::dial`]). On connect the hub checks the claimed target against the
//! control plane, takes a lease on it through [`MachineControl`] (the only writer of leases), tells the
//! worker which lease it holds, and keeps it renewed. A dispatch is forwarded only while that lease is
//! live in the database, so the fence the worker applies is also applied here, before anything is sent.
//!
//! What the hub never does: dial a worker, guess a worker for a workspace that has none (that call is
//! refused, never simulated), or retry a dispatch whose result it did not see. A dispatch that was sent and
//! not answered settles as *unknown*, and the runtime reconciles it.

mod host;
mod hub;

pub use host::{WorkerRoutedHost, WorkerToolHost, WORKER_TOOLS};
pub use hub::{DispatchArgs, HubConfig, HubError, LinkSnapshot, WorkerHub};
