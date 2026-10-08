//! A bounded record of dispatch tokens, so a re-delivered dispatch runs at most once (EXEC-002, ADP-009).
//!
//! A dispatch token names one effect. The first delivery runs; a delivery of a token that finished is answered with
//! what it recorded; a delivery of a token that was started and has no recorded outcome (the worker stopped, or the
//! future was dropped, after the work may have begun) is reported as such and never run again. The record is in
//! memory: a worker that restarted has no record, and the runtime's Effect Ledger — which already holds the effect
//! `DISPATCHED` — is what reconciles it.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Mutex, PoisonError};

/// What the journal knows about a token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Replay<T> {
    /// The token finished; this is what it produced.
    Done(T),
    /// The token started and has no recorded outcome.
    Started,
    /// The token is new.
    Fresh,
}

#[derive(Debug)]
struct Inner<T> {
    done: BTreeMap<String, T>,
    order: VecDeque<String>,
    started: BTreeSet<String>,
}

/// The journal.
#[derive(Debug)]
pub struct ReplayJournal<T> {
    capacity: usize,
    inner: Mutex<Inner<T>>,
}

impl<T: Clone> ReplayJournal<T> {
    /// A journal that keeps the outcomes of its last `capacity` tokens.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            inner: Mutex::new(Inner {
                done: BTreeMap::new(),
                order: VecDeque::new(),
                started: BTreeSet::new(),
            }),
        }
    }

    /// What is known about `token`.
    #[must_use]
    pub fn lookup(&self, token: &str) -> Replay<T> {
        let inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(done) = inner.done.get(token) {
            return Replay::Done(done.clone());
        }
        if inner.started.contains(token) {
            return Replay::Started;
        }
        Replay::Fresh
    }

    /// Mark `token` started. False when it already was (or had finished), in which case nothing must run.
    pub fn start(&self, token: &str) -> bool {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if inner.done.contains_key(token) {
            return false;
        }
        inner.started.insert(token.to_string())
    }

    /// Take back a start whose work provably never began, so the token may be delivered again.
    pub fn abandon(&self, token: &str) {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .started
            .remove(token);
    }

    /// Record the outcome of `token`.
    pub fn finish(&self, token: &str, outcome: T) {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        inner.started.remove(token);
        if inner.done.insert(token.to_string(), outcome).is_none() {
            inner.order.push_back(token.to_string());
        }
        while inner.order.len() > self.capacity {
            if let Some(oldest) = inner.order.pop_front() {
                inner.done.remove(&oldest);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_runs_once_and_is_answered_from_the_record_after() {
        let journal: ReplayJournal<u32> = ReplayJournal::new(2);
        assert_eq!(journal.lookup("a"), Replay::Fresh);
        assert!(journal.start("a"));
        assert_eq!(journal.lookup("a"), Replay::Started);
        assert!(
            !journal.start("a"),
            "a token that started does not start again"
        );
        journal.finish("a", 7);
        assert_eq!(journal.lookup("a"), Replay::Done(7));
        assert!(
            !journal.start("a"),
            "a token that finished does not start again"
        );
    }

    #[test]
    fn the_record_is_bounded_oldest_first() {
        let journal: ReplayJournal<u32> = ReplayJournal::new(2);
        for (token, value) in [("a", 1), ("b", 2), ("c", 3)] {
            assert!(journal.start(token));
            journal.finish(token, value);
        }
        assert_eq!(journal.lookup("a"), Replay::Fresh);
        assert_eq!(journal.lookup("b"), Replay::Done(2));
        assert_eq!(journal.lookup("c"), Replay::Done(3));
    }
}
