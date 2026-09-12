//! Bounded-concurrency dispatch for independent tool calls (DOMAIN.md §7.4).
//!
//! Parallel tool calls are *the model's* parallelism: the runtime decides which proposed
//! calls may run together and always applies their results to the Step rows in proposal
//! order, so the durable record does not depend on completion order.
//!
//! [`join_all`] is deliberately dependency-free. Adding a futures runtime (or a second
//! executor) to `crates/server` for one fan-out is not justified by DOSSIER.md §23 change
//! control; polling several boxed futures on the task that already drives the turn gives
//! the concurrency that matters here, which is overlapping I/O against the tool hosts.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context as TaskContext, Poll};

use quansio_tools::canonical_json;

use crate::runtime::state_machine::ProposedToolCall;

/// Tools that mutate runtime state or park the run. They are dispatched one per round, in
/// proposal order, after the independent batch, because their result decides the run's
/// next state.
pub const CONTROL_TOOLS: [&str; 4] = [
    "user.ask",
    "work.delegate",
    "work.propose_plan",
    "memory.propose",
];

/// Whether a proposed call is control-plane rather than a side-effecting tool call.
#[must_use]
pub fn is_control_tool(name: &str) -> bool {
    CONTROL_TOOLS.contains(&name)
}

/// Poll every future to completion on the current task, preserving input order.
///
/// The futures must be `Send` because the turn loop's future is; they are polled
/// in turn on one task, so a slow host does not block a fast one.
pub async fn join_all<'a, T>(futures: Vec<Pin<Box<dyn Future<Output = T> + Send + 'a>>>) -> Vec<T> {
    let mut slots: Vec<Option<Pin<Box<dyn Future<Output = T> + Send + 'a>>>> =
        futures.into_iter().map(Some).collect();
    let mut results: Vec<Option<T>> = (0..slots.len()).map(|_| None).collect();

    std::future::poll_fn(|cx: &mut TaskContext<'_>| {
        let mut pending = false;
        for (index, slot) in slots.iter_mut().enumerate() {
            let Some(future) = slot else {
                continue;
            };
            match future.as_mut().poll(cx) {
                Poll::Ready(value) => {
                    results[index] = Some(value);
                    *slot = None;
                }
                Poll::Pending => pending = true,
            }
        }
        if pending {
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    })
    .await;

    results
        .into_iter()
        .map(|value| value.expect("join_all resolves every future it was given"))
        .collect()
}

/// Group proposed calls into dispatch rounds.
///
/// A call runs concurrently with the others in its round; rounds are applied in order.
/// Two calls are separated into different rounds when one of them is a [`CONTROL_TOOLS`]
/// member (which gets a round of its own, after the independent batch) or when another
/// call in the round proposes the identical tool with identical arguments — the second
/// occurrence goes to the next round so the pair cannot race for one idempotency key.
///
/// Calls that touch the same resource with *different* arguments are not ordered by this
/// function; the Effect Ledger still refuses a second in-flight reservation of one
/// idempotency key, and the host is responsible for its own resource locking.
#[must_use]
pub fn plan_rounds(calls: &[ProposedToolCall]) -> Vec<Vec<usize>> {
    let mut rounds: Vec<Vec<usize>> = Vec::new();
    let mut seen: Vec<(String, String)> = Vec::new();

    for (index, call) in calls.iter().enumerate() {
        if is_control_tool(&call.tool) {
            continue;
        }
        let key = (call.tool.clone(), canonical_json(&call.args));
        let occurrence = seen.iter().filter(|existing| **existing == key).count();
        seen.push(key);
        while rounds.len() <= occurrence {
            rounds.push(Vec::new());
        }
        rounds[occurrence].push(index);
    }

    for (index, call) in calls.iter().enumerate() {
        if is_control_tool(&call.tool) {
            rounds.push(vec![index]);
        }
    }

    rounds.retain(|round| !round.is_empty());
    rounds
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{is_control_tool, plan_rounds, ProposedToolCall};

    fn call(id: &str, tool: &str, args: serde_json::Value) -> ProposedToolCall {
        ProposedToolCall::new(id, tool, args)
    }

    #[test]
    fn independent_calls_share_a_round_and_keep_proposal_order() {
        let calls = vec![
            call("c1", "fs.read", json!({ "path": "/a" })),
            call("c2", "terminal.exec", json!({ "command": "ls" })),
            call("c3", "fs.read", json!({ "path": "/b" })),
        ];
        assert_eq!(plan_rounds(&calls), vec![vec![0, 1, 2]]);
    }

    #[test]
    fn identical_calls_are_serialised() {
        let calls = vec![
            call("c1", "fs.read", json!({ "path": "/a" })),
            call("c2", "fs.read", json!({ "path": "/a" })),
            call("c3", "fs.read", json!({ "path": "/a" })),
        ];
        assert_eq!(plan_rounds(&calls), vec![vec![0], vec![1], vec![2]]);
    }

    #[test]
    fn control_tools_run_alone_and_last() {
        let calls = vec![
            call("c1", "user.ask", json!({ "prompt": "?" })),
            call("c2", "fs.read", json!({ "path": "/a" })),
        ];
        assert_eq!(plan_rounds(&calls), vec![vec![1], vec![0]]);
        assert!(is_control_tool("work.delegate"));
        assert!(!is_control_tool("fs.read"));
    }

    #[test]
    fn no_calls_means_no_rounds() {
        assert!(plan_rounds(&[]).is_empty());
    }
}
