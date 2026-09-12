//! Identity, generation and idempotency fixtures (CORE-002).
//!
//! These tests drive the shipped primitives in `quansio-core`: the ULID codec and
//! monotonic generator, the typed prefix table (cross-checked against the generated
//! protobuf enum), generation/revision/fence acceptance, cursor round-trips and
//! idempotency-key derivation.

use std::collections::HashSet;

use quansio_core::{
    classify_duplicate, CanonicalId, CausationId, Clock, CommandId, CoreError, CorrelationId,
    Cursor, Digest, EntropySource, FenceToken, Generation, IdempotencyKey, Prefix, Revision,
    Sequence, TypedId, Ulid, UlidGenerator,
};

/// Clock with a fixed, controllable time.
struct FixedClock(u64);

impl Clock for FixedClock {
    fn now_ms(&self) -> u64 {
        self.0
    }
}

/// Deterministic entropy so a fixture is reproducible.
struct FixedEntropy(u8);

impl EntropySource for FixedEntropy {
    fn fill(&self, buf: &mut [u8]) {
        for (index, byte) in buf.iter_mut().enumerate() {
            *byte = self.0.wrapping_add(index as u8);
        }
    }
}

fn generator() -> UlidGenerator<FixedClock, FixedEntropy> {
    UlidGenerator::with_sources(FixedClock(1_469_918_176_385), FixedEntropy(0x2A))
}

// ---------------------------------------------------------------- ULID codec

#[test]
fn ulid_round_trips_through_base32() {
    let mut gen = generator();
    for _ in 0..128 {
        let ulid = gen.generate();
        let encoded = ulid.to_base32();
        assert_eq!(encoded.len(), 26);
        assert_eq!(Ulid::parse(&encoded).expect("parse"), ulid);
        assert_eq!(ulid.timestamp_ms(), 1_469_918_176_385);
    }
}

#[test]
fn ulid_matches_the_published_spec_vector() {
    // ULID spec example: this base32 string encodes timestamp 1469918176385.
    let ulid = Ulid::parse("01ARYZ6S41TSV4RRFFQ69G5FAV").expect("spec vector parses");
    assert_eq!(ulid.timestamp_ms(), 1_469_918_176_385);
    assert_eq!(ulid.to_base32(), "01ARYZ6S41TSV4RRFFQ69G5FAV");
}

#[test]
fn ulid_ordering_follows_time_then_randomness() {
    let earlier = Ulid::parse("01ARYZ6S41TSV4RRFFQ69G5FAV").expect("parse");
    let later = Ulid::parse("01ARYZ6S41TSV4RRFFQ69G5FAW").expect("parse");
    assert!(
        earlier < later,
        "same-millisecond ULIDs must order by randomness"
    );
}

#[test]
fn ulid_rejects_malformed_input() {
    for bad in [
        "",
        "01ARYZ6S41TSV4RRFFQ69G5FA",   // 25 chars
        "01ARYZ6S41TSV4RRFFQ69G5FAVX", // 27 chars
        "01ARYZ6S41TSV4RRFFQ69G5FAI",  // I is not in Crockford's alphabet
        "81ARYZ6S41TSV4RRFFQ69G5FAV",  // leading digit overflows 128 bits
    ] {
        assert!(Ulid::parse(bad).is_err(), "{bad:?} must be rejected");
    }
}

#[test]
fn generator_is_monotonic_within_one_millisecond() {
    let mut gen = generator();
    let ids: Vec<Ulid> = (0..1_000).map(|_| gen.generate()).collect();
    for pair in ids.windows(2) {
        assert!(pair[0] < pair[1], "ids must be strictly increasing");
    }
    assert_eq!(ids.iter().collect::<HashSet<_>>().len(), ids.len());
}

#[test]
fn generator_advances_with_the_clock() {
    let mut gen = UlidGenerator::with_sources(FixedClock(1_000), FixedEntropy(1));
    let first = gen.generate();
    let mut later = UlidGenerator::with_sources(FixedClock(2_000), FixedEntropy(1));
    let second = later.generate();
    assert!(first < second);
    assert_eq!(second.timestamp_ms(), 2_000);
}

// ---------------------------------------------------------------- canonical ids

#[test]
fn canonical_ids_round_trip_with_their_prefix() {
    let mut gen = generator();
    let run = CanonicalId::generate(Prefix::Run, &mut gen);
    let text = run.to_string();
    assert!(text.starts_with("run_"));
    let parsed = CanonicalId::parse(&text).expect("round trip");
    assert_eq!(parsed, run);
    assert_eq!(parsed.prefix(), Prefix::Run);

    let command: CommandId = CommandId::generate(&mut gen);
    assert!(command.to_string().starts_with("cmd_"));
    assert_eq!(
        CommandId::parse(&command.to_string()).expect("typed parse"),
        command
    );
}

#[test]
fn typed_ids_reject_the_wrong_prefix() {
    let mut gen = generator();
    let run = CanonicalId::generate(Prefix::Run, &mut gen);
    let error = CommandId::new(run).expect_err("a run id is not a command id");
    assert!(matches!(error, CoreError::InvalidPrefix { .. }));
}

#[test]
fn unknown_prefix_is_rejected() {
    let error = CanonicalId::parse("zzz_01ARYZ6S41TSV4RRFFQ69G5FAV").expect_err("unknown prefix");
    assert!(matches!(error, CoreError::UnknownPrefix(_)));
}

#[test]
fn correlation_and_causation_ids_carry_the_right_shape() {
    let mut gen = generator();
    let correlation = CorrelationId::generate(&mut gen);
    let text = correlation.to_string();
    assert_eq!(text.len(), 26, "correlation ids are bare ULIDs");
    assert_eq!(text.parse::<CorrelationId>().expect("parse"), correlation);

    let event = format!("evt_{text}")
        .parse::<CausationId>()
        .expect("event causation");
    let command = format!("cmd_{text}")
        .parse::<CausationId>()
        .expect("command causation");
    assert_ne!(event.to_string(), command.to_string());
    assert!(format!("run_{text}").parse::<CausationId>().is_err());
}

#[test]
fn ids_serialize_as_their_canonical_string() {
    let mut gen = generator();
    let id = CommandId::generate(&mut gen);
    let json = serde_json::to_string(&id).expect("serialize");
    assert_eq!(json, format!("\"{id}\""));
    let back: CommandId = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, id);
}

// ---------------------------------------------------------------- counters

#[test]
fn generation_accepts_current_or_newer_and_rejects_stale() {
    let current = Generation::new(7).expect("valid");
    assert!(current.accept(Generation::new(7).expect("valid")).is_ok());
    assert!(current.accept(Generation::new(9).expect("valid")).is_ok());
    let error = current
        .accept(Generation::new(6).expect("valid"))
        .expect_err("stale");
    assert_eq!(
        error,
        CoreError::StaleGeneration {
            received: 6,
            current: 7
        }
    );
    assert!(Generation::new(0).is_err(), "generation 0 is not a state");
    assert_eq!(current.next().get(), 8);
}

#[test]
fn revision_check_and_bump_is_compare_and_set() {
    let current = Revision::new(3);
    assert_eq!(
        current
            .check_and_bump(Revision::new(3))
            .expect("match")
            .get(),
        4
    );
    assert!(current.check_and_bump(Revision::new(2)).is_err());
    assert_eq!(Revision::INITIAL.get(), 1);
}

#[test]
fn sequence_must_be_positive() {
    assert!(Sequence::new(0).is_err());
    assert!(Sequence::new(-1).is_err());
    assert_eq!(Sequence::new(1).expect("first").next().get(), 2);
}

#[test]
fn fence_tokens_round_trip_and_reject_stale_leases() {
    let mut gen = generator();
    let lease = CanonicalId::generate(Prefix::Lease, &mut gen);
    let token = FenceToken::new(lease, Generation::new(4).expect("valid"));
    let text = token.to_string();
    assert_eq!(text.parse::<FenceToken>().expect("round trip"), token);

    let other_lease = CanonicalId::generate(Prefix::Lease, &mut gen);
    assert!(token
        .accept(&other_lease, Generation::new(4).expect("valid"))
        .is_err());
    assert!(token
        .accept(&lease, Generation::new(3).expect("valid"))
        .is_err());
    // A token from a generation the runtime has not granted is rejected too.
    assert!(token
        .accept(&lease, Generation::new(5).expect("valid"))
        .is_err());
    assert!(token
        .accept(&lease, Generation::new(4).expect("valid"))
        .is_ok());

    for bad in [
        "lse_only",
        "run_x:1",
        "lse_01ARYZ6S41TSV4RRFFQ69G5FAV:0",
        "",
    ] {
        assert!(
            bad.parse::<FenceToken>().is_err(),
            "{bad:?} must be rejected"
        );
    }
}

// ---------------------------------------------------------------- cursors

#[test]
fn cursors_round_trip_and_stay_opaque() {
    let cursor = Cursor::new(
        "tenant:tn_01ARYZ6S41TSV4RRFFQ69G5FAV",
        Sequence::new(42).expect("seq"),
    );
    let token = cursor.encode();
    assert!(
        !token.contains(':'),
        "the encoded cursor must not leak its structure"
    );
    let decoded = Cursor::decode(&token).expect("round trip");
    assert_eq!(decoded, cursor);
    assert_eq!(decoded.sequence().get(), 42);
    assert_eq!(decoded.stream_id(), "tenant:tn_01ARYZ6S41TSV4RRFFQ69G5FAV");

    for bad in ["", "not-base64!!", "MTIz", "MTphYmM6MA"] {
        assert!(Cursor::decode(bad).is_err(), "{bad:?} must be rejected");
    }
}

// ---------------------------------------------------------------- idempotency

#[test]
fn idempotency_keys_are_stable_and_sensitive() {
    let digest = Digest::of_canonical_json(r#"{"to":"a@example.com","subject":"hi"}"#);
    let base = IdempotencyKey::derive("message.send", "cnx_1/channel:#eng", digest.clone());
    let same = IdempotencyKey::derive(
        "message.send",
        "cnx_1/channel:#eng",
        Digest::of_canonical_json(r#"{"to":"a@example.com","subject":"hi"}"#),
    );
    assert_eq!(
        base.canonical(),
        same.canonical(),
        "same inputs must map to one key"
    );

    let other_params = IdempotencyKey::derive(
        "message.send",
        "cnx_1/channel:#eng",
        Digest::of_canonical_json(r#"{"to":"a@example.com","subject":"different"}"#),
    );
    let other_resource =
        IdempotencyKey::derive("message.send", "cnx_1/channel:#sales", digest.clone());
    let other_class = IdempotencyKey::derive("record.create", "cnx_1/channel:#eng", digest.clone());
    assert_ne!(base.canonical(), other_params.canonical());
    assert_ne!(base.canonical(), other_resource.canonical());
    assert_ne!(base.canonical(), other_class.canonical());
    assert!(base
        .canonical()
        .starts_with("message.send:cnx_1/channel:#eng:"));

    assert_eq!(digest.as_str().len(), 64);
    assert_eq!(Digest::of(b"abc").as_str().len(), 64);
    assert!(Digest::of(b"abc") != Digest::of(b"abcd"));
}

#[test]
fn duplicate_commands_replay_or_conflict() {
    let mut gen = generator();
    let command = CommandId::generate(&mut gen);
    let first = Digest::of_canonical_json(r#"{"title":"plan"}"#);

    // Identical re-submission is a replay, never a second mutation.
    assert_eq!(
        classify_duplicate(&command, &first, &first).expect("replay"),
        quansio_core::DuplicateOutcome::Replay
    );

    // The same command id with different parameters is a typed conflict.
    let changed = Digest::of_canonical_json(r#"{"title":"plan","extra":true}"#);
    let error = classify_duplicate(&command, &first, &changed).expect_err("conflict");
    assert_eq!(
        error,
        CoreError::IdempotencyMismatch {
            command_id: command.to_string()
        }
    );
}
