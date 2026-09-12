//! The ToolCall protocol, human question protocol and turn-loop dispatch (RUN-011).
//!
//! Canonical owner: `crates/server/src/runtime/turn_loop`. `crates/tools` owns the Tool
//! contract and Tool Registry (what a tool is); this module owns what happens to a
//! proposed call (DOMAIN.md §5.6, §7.4) and the `user.ask` protocol (DOMAIN.md §3.4).
//!
//! Everything here composes existing primitives: capability projections (RUN-005), policy
//! and approvals (RUN-006), the Effect Ledger (RUN-007), the Run/Turn/Step/Attempt state
//! machine (RUN-001), delegation (RUN-002) and questions. Nothing opens a second store,
//! effect path or orchestration loop.

pub mod dispatch;
pub mod parallel;
pub mod questions;
pub mod usage;

pub use dispatch::{
    is_internal_tool, ActingActor, HostDispatch, HostFailure, HostOutcome, MemoryProposalContext,
    MemoryProposalPort, PlanApplicationContext, PlanApplicationPort, ProjectionProvider,
    ProposalTrustSource, RoleProvider, StoredProjectionProvider, ToolDispatchService, ToolHostPort,
    UnavailableMemoryProposals, UnavailablePlanApplication, UnavailableProposalTrust,
    UnavailableRoles, UnavailableToolHost, INTERNAL_TOOLS,
};
pub use parallel::{is_control_tool, join_all, plan_rounds, CONTROL_TOOLS};
pub use questions::{
    AnsweredQuestion, NewQuestion, QuestionAnswer, QuestionRecord, QuestionService, QuestionStatus,
};
pub use usage::{load_turn_usage, ToolUsageEntry, TurnUsage};
