import XCTest

@testable import QuansioMacBridge

/// The recorder session (ADP-003, DOMAIN.md 11.7 A-C): redaction before storage, the secure-input pause, the
/// owner/epoch fence, and a recording that only the person's explicit finish completes.
final class RecorderSessionTests: XCTestCase {
    private var clock = ManualClock()
    private var secure = ScriptedSecureInput()

    override func setUp() {
        clock = ManualClock()
        secure = ScriptedSecureInput()
    }

    private func session(
        limits: RecordingLimits = RecordingLimits(), sampler: ScreenshotSampler? = nil
    ) -> RecorderSession {
        RecorderSession(
            recordingId: testRecording, owner: testOwner, limits: limits, clock: clock, secureInput: secure,
            sampler: sampler)
    }

    private func archive(_ finished: FinishedArchive) throws -> [String: Any] { try jsonObject(finished.data) }

    // MARK: redaction before anything is stored

    func testNothingTheRulesRemoveIsKeptAndTheSummaryHoldsCountsOnly() throws {
        let s = session()
        clock.advance(seconds: 1)
        _ = try s.record(observation(.type, at: clock, role: "AXTextField", title: "Notes", text: "key AKIAIOSFODNN7EXAMPLE saved"), owner: testOwner)
        clock.advance(seconds: 1)
        _ = try s.record(observation(.type, at: clock, role: "AXTextField", title: "Card number", text: "4111 1111 1111 1111"), owner: testOwner)
        clock.advance(seconds: 1)
        _ = try s.record(observation(.click, at: clock, title: "Pay with 4111111111111111", window: "Bearer abcdefgh12345678"), owner: testOwner)
        try s.appendTranscript("my verification code is 482913 and AKIAIOSFODNN7EXAMPL2", owner: testOwner)
        clock.advance(seconds: 1)
        let finished = try s.finish(owner: testOwner, requestId: "fin_1")
        let text = try XCTUnwrap(String(data: finished.data, encoding: .utf8))
        for secret in ["AKIAIOSFODNN7EXAMPLE", "AKIAIOSFODNN7EXAMPL2", "4111", "abcdefgh", "482913"] {
            XCTAssertFalse(text.contains(secret), "\(secret) reached the archive")
        }
        let document = try archive(finished)
        let redaction = try XCTUnwrap(document["redaction"] as? [String: Any])
        let counts = try XCTUnwrap(redaction["counts"] as? [String: Int])
        XCTAssertEqual(counts["api_key"], 2)
        XCTAssertEqual(counts["card_number"], 2)
        XCTAssertEqual(counts["bearer_token"], 1)
        XCTAssertEqual(counts["verification_code"], 1)
        XCTAssertEqual(counts["password_field"], 0)
        XCTAssertEqual(redaction["rule_set"] as? String, "v1")
        let actions = try XCTUnwrap(document["actions"] as? [[String: Any]])
        XCTAssertEqual(actions[0]["text"] as? String, "key [REDACTED:api_key] saved")
        XCTAssertEqual(actions[1]["text"] as? String, "[REDACTED:card_number]", "a labelled field's text is replaced whole")
    }

    func testRedactionRunsBeforeTruncationSoACutCannotLeaveHalfASecret() throws {
        var limits = RecordingLimits()
        limits.actionTextMaxChars = 20
        let s = session(limits: limits)
        clock.advance(seconds: 1)
        _ = try s.record(observation(.type, at: clock, role: "AXTextField", title: "Body", text: "AKIAIOSFODNN7EXAMPLE and more words"), owner: testOwner)
        let finished = try s.finish(owner: testOwner, requestId: "fin_1")
        let actions = try XCTUnwrap(try archive(finished)["actions"] as? [[String: Any]])
        let kept = try XCTUnwrap(actions[0]["text"] as? String)
        XCTAssertEqual(kept, String("[REDACTED:api_key] and more words".prefix(20)))
        XCTAssertFalse(kept.contains("AKIA"))
    }

    // MARK: secure fields and secure input

    func testTextTypedIntoASecureFieldIsDroppedWholeAndCounted() throws {
        let s = session()
        clock.advance(seconds: 1)
        let outcome = try s.record(
            observation(.type, at: clock, role: "AXTextField", title: "Password", text: "hunter2", subrole: "AXSecureTextField"),
            owner: testOwner)
        guard case .recorded = outcome else { return XCTFail("a typing burst into a secure field is recorded without its text") }
        // Keystrokes in a secure field are dropped without a trace beyond the pause.
        clock.advance(seconds: 1)
        let key = try s.record(
            observation(.key, at: clock, role: "AXTextField", key: "a", subrole: "AXSecureTextField"), owner: testOwner)
        XCTAssertEqual(key, .droppedSensitive)
        clock.advance(seconds: 1)
        _ = try s.record(observation(.click, at: clock, title: "Sign in"), owner: testOwner)
        let finished = try s.finish(owner: testOwner, requestId: "fin_1")
        XCTAssertFalse(String(decoding: finished.data, as: UTF8.self).contains("hunter2"))
        let document = try archive(finished)
        let actions = try XCTUnwrap(document["actions"] as? [[String: Any]])
        XCTAssertEqual(actions.count, 2, "the secure typing burst and the click; the keystroke left nothing")
        XCTAssertNil(actions[0]["text"])
        XCTAssertEqual(actions[0]["secure"] as? Bool, true)
        let counts = try XCTUnwrap((document["redaction"] as? [String: Any])?["counts"] as? [String: Int])
        XCTAssertEqual(counts["password_field"], 1)
        let paused = try XCTUnwrap(document["paused"] as? [[String: Any]])
        XCTAssertEqual(paused.count, 1)
        XCTAssertEqual(paused[0]["reason"] as? String, "secure_field")
    }

    func testCaptureStopsWhileSecureInputIsOnAndResumesAfter() throws {
        var captures = 0
        let sampler = ScreenshotSampler(limits: RecordingLimits(screenshotMinIntervalMs: 0), recognizer: FixedRecognizer(text: "Notes")) {
            captures += 1
            return makePNG(width: 64, height: 48, red: CGFloat(captures) / 10)
        }
        let s = session(sampler: sampler)
        clock.advance(seconds: 1)
        XCTAssertEqual(try s.tickScreenshot(owner: testOwner), .kept)
        secure.set(true)
        clock.advance(seconds: 1)
        XCTAssertEqual(try s.record(observation(.type, at: clock, role: "AXTextField", text: "swordfish"), owner: testOwner), .paused(reason: "secure_input"))
        XCTAssertEqual(try s.tickScreenshot(owner: testOwner), .unavailable)
        XCTAssertEqual(captures, 1, "no screenshot is even taken while secure input is on")
        XCTAssertEqual(try s.status(owner: testOwner).pausedReason, "secure_input")
        secure.set(false)
        clock.advance(seconds: 1)
        XCTAssertEqual(try s.record(observation(.click, at: clock, title: "Done"), owner: testOwner), .recorded(seq: 1))
        clock.advance(seconds: 1)
        let finished = try s.finish(owner: testOwner, requestId: "fin_1")
        XCTAssertFalse(String(decoding: finished.data, as: UTF8.self).contains("swordfish"))
        let paused = try XCTUnwrap(try archive(finished)["paused"] as? [[String: Any]])
        XCTAssertEqual(paused.map { $0["reason"] as? String }, ["secure_input"])
    }

    // MARK: the owner and epoch fence

    func testAnotherAccountOrEpochEndsTheSessionAndDiscardsEverything() throws {
        let others = [
            RecordingOwner(tenantId: testOwner.tenantId, userId: "usr_01J8Z3K6F1N8VQ2X5W9YOTHER1", epoch: testOwner.epoch),
            RecordingOwner(tenantId: "tn_01J8Z3K6F1N8VQ2X5W9YOTHER1", userId: testOwner.userId, epoch: testOwner.epoch),
            RecordingOwner(tenantId: testOwner.tenantId, userId: testOwner.userId, epoch: testOwner.epoch + 1),
        ]
        for other in others {
            let s = session()
            clock.advance(seconds: 1)
            _ = try s.record(observation(.click, at: clock, title: "New Note"), owner: testOwner)
            try s.appendTranscript("buffered narration", owner: testOwner)
            XCTAssertThrowsError(try s.record(observation(.click, at: clock, title: "Save"), owner: other)) {
                XCTAssertEqual($0 as? RecorderError, .ownerChanged)
            }
            XCTAssertEqual(s.currentState, .aborted("owner_changed"))
            // The right owner finds a recording that is over, with nothing left to finish.
            XCTAssertThrowsError(try s.finish(owner: testOwner, requestId: "fin_1")) {
                XCTAssertEqual($0 as? RecorderError, .notRecording(state: "aborted:owner_changed"))
            }
            for call in [
                { try s.status(owner: testOwner) }, { try s.status(owner: other) },
            ] {
                XCTAssertThrowsError(try call())
            }
        }
    }

    func testEveryCallCarriesTheFence() throws {
        let other = RecordingOwner(tenantId: testOwner.tenantId, userId: testOwner.userId, epoch: 99)
        let calls: [(String, (RecorderSession) throws -> Void)] = [
            ("record", { _ = try $0.record(observation(.click, at: self.clock), owner: other) }),
            ("transcript", { try $0.appendTranscript("hello", owner: other) }),
            ("screenshot", { _ = try $0.tickScreenshot(owner: other) }),
            ("status", { _ = try $0.status(owner: other) }),
            ("finish", { _ = try $0.finish(owner: other, requestId: "fin") }),
            ("cancel", { try $0.cancel(owner: other) }),
        ]
        for (name, call) in calls {
            let s = session()
            XCTAssertThrowsError(try call(s), name) { XCTAssertEqual($0 as? RecorderError, .ownerChanged, name) }
            XCTAssertEqual(s.currentState, .aborted("owner_changed"), name)
        }
    }

    // MARK: a recording completes only when the person finishes it

    func testNothingButFinishCompletesARecording() throws {
        var limits = RecordingLimits()
        limits.maxActions = 3
        limits.transcriptMaxSegments = 2
        let s = session(limits: limits)
        for index in 0..<6 {
            clock.advance(seconds: 60)
            let outcome = try s.record(observation(.click, at: clock, title: "B\(index)"), owner: testOwner)
            if index >= 3 { XCTAssertEqual(outcome, .droppedForLimit) }
        }
        for _ in 0..<5 { try s.appendTranscript("more narration", owner: testOwner) }
        // Every limit is spent and a long time has passed: it is still a recording in progress.
        XCTAssertEqual(s.currentState, .recording)
        let status = try s.status(owner: testOwner)
        XCTAssertEqual(status.state, "recording")
        XCTAssertEqual(status.actions, 3)
        XCTAssertEqual(status.droppedForLimit, 3)
        XCTAssertEqual(status.transcriptSegments, 2)
        let finished = try s.finish(owner: testOwner, requestId: "fin_1")
        XCTAssertEqual(s.currentState, .finished)
        XCTAssertEqual(finished.actions, 3)
        let document = try archive(finished)
        XCTAssertEqual((document["finish"] as? [String: Any])?["by"] as? String, "user")
        XCTAssertEqual((document["finish"] as? [String: Any])?["request_id"] as? String, "fin_1")
    }

    func testRunningOutOfTimeAbortsAndDiscardsItNeverFinishes() throws {
        var limits = RecordingLimits()
        limits.maxDurationMs = 60_000
        let s = session(limits: limits)
        clock.advance(seconds: 10)
        _ = try s.record(observation(.click, at: clock, title: "New Note"), owner: testOwner)
        clock.advance(seconds: 120)
        XCTAssertThrowsError(try s.finish(owner: testOwner, requestId: "fin_1")) {
            XCTAssertEqual($0 as? RecorderError, .expired)
        }
        XCTAssertEqual(s.currentState, .aborted("expired"))
        XCTAssertThrowsError(try s.status(owner: testOwner)) {
            XCTAssertEqual($0 as? RecorderError, .notRecording(state: "aborted:expired"))
        }
    }

    func testCancelDiscardsAndAFinishedSessionCannotBeFinishedAgainDifferently() throws {
        let cancelled = session()
        _ = try cancelled.record(observation(.click, at: clock, title: "New Note"), owner: testOwner)
        try cancelled.cancel(owner: testOwner)
        XCTAssertEqual(cancelled.currentState, .cancelled)
        XCTAssertThrowsError(try cancelled.finish(owner: testOwner, requestId: "fin_1"))

        let s = session()
        clock.advance(seconds: 1)
        _ = try s.record(observation(.click, at: clock, title: "New Note"), owner: testOwner)
        clock.advance(seconds: 1)
        let first = try s.finish(owner: testOwner, requestId: "fin_1")
        XCTAssertEqual(try s.finish(owner: testOwner, requestId: "fin_1"), first, "the same request id is the same archive")
        XCTAssertThrowsError(try s.finish(owner: testOwner, requestId: "fin_2"))
        XCTAssertThrowsError(try s.record(observation(.click, at: clock), owner: testOwner))
    }

    // MARK: limits

    func testAnArchiveOverItsLimitDropsScreenshotsThenRefuses() throws {
        var limits = RecordingLimits(screenshotMinIntervalMs: 0)
        limits.archiveMaxBytes = 3_000
        var shot = 0
        let sampler = ScreenshotSampler(limits: limits, recognizer: FixedRecognizer(text: "Notes")) {
            shot += 1
            return makePNG(width: 96, height: 96, red: CGFloat(shot) / 20, green: 0.1, blue: 0.9)
        }
        let s = session(limits: limits, sampler: sampler)
        for _ in 0..<4 {
            clock.advance(seconds: 1)
            _ = try s.tickScreenshot(owner: testOwner)
        }
        XCTAssertGreaterThan(sampler.kept.count, 0)
        clock.advance(seconds: 1)
        _ = try s.record(observation(.click, at: clock, title: "New Note"), owner: testOwner)
        let finished = try s.finish(owner: testOwner, requestId: "fin_1")
        XCTAssertLessThanOrEqual(finished.data.count, 3_000)

        var tiny = RecordingLimits()
        tiny.archiveMaxBytes = 200
        let refused = session(limits: tiny)
        _ = try refused.record(observation(.click, at: clock, title: "New Note"), owner: testOwner)
        XCTAssertThrowsError(try refused.finish(owner: testOwner, requestId: "fin_1")) {
            guard case .archiveTooLarge = $0 as? RecorderError else { return XCTFail("\($0)") }
        }
        XCTAssertEqual(refused.currentState, .recording, "a refused finish leaves the recording in progress")
    }

    func testAScreenPointIsKeptOnlyForAnUnresolvedControlAsEvidence() throws {
        let s = session()
        clock.advance(seconds: 1)
        var unresolved = observation(.click, at: clock)
        unresolved.target = nil
        unresolved.targetUnresolved = true
        unresolved.point = (412.5, 98)
        _ = try s.record(unresolved, owner: testOwner)
        var resolved = observation(.click, at: clock, title: "Save")
        resolved.point = (1, 2)
        _ = try s.record(resolved, owner: testOwner)
        let finished = try s.finish(owner: testOwner, requestId: "fin_1")
        let actions = try XCTUnwrap(try archive(finished)["actions"] as? [[String: Any]])
        XCTAssertNotNil(actions[0]["point"])
        XCTAssertEqual(actions[0]["target_unresolved"] as? Bool, true)
        XCTAssertNil(actions[1]["point"], "a resolved control keeps no coordinates")
    }

    // MARK: the archive's shape, against what the server accepts

    func testTheArchiveHasExactlyTheShapeTheServerValidates() throws {
        let sampler = ScreenshotSampler(limits: RecordingLimits(screenshotMinIntervalMs: 0), recognizer: FixedRecognizer(text: "Notes")) {
            makePNG(width: 64, height: 48)
        }
        let s = session(sampler: sampler)
        clock.advance(seconds: 1)
        _ = try s.record(observation(.type, at: clock, role: "AXTextArea", title: "Note body", text: "Quarterly report"), owner: testOwner)
        clock.advance(seconds: 1)
        _ = try s.tickScreenshot(owner: testOwner)
        clock.advance(seconds: 1)
        let document = try archive(try s.finish(owner: testOwner, requestId: "fin_1"))
        XCTAssertEqual(
            Set(document.keys),
            ["schema_version", "recording_id", "owner", "platform", "started_at", "finished_at", "finish", "apps", "actions", "screenshots", "transcript", "paused", "redaction", "limits"])
        let shot = try XCTUnwrap((document["screenshots"] as? [[String: Any]])?.first)
        XCTAssertEqual(
            Set(shot.keys), ["seq", "at", "sha256", "bytes", "media_type", "width", "height", "data_base64", "screen_text"])
        XCTAssertEqual(shot["media_type"] as? String, "image/png")
        let action = try XCTUnwrap((document["actions"] as? [[String: Any]])?.first)
        XCTAssertEqual(Set(action.keys), ["seq", "at", "kind", "app", "target", "text"])
        XCTAssertEqual(document["owner"] as? [String: AnyHashable], testOwner.wire as? [String: AnyHashable])
    }

    /// The archive a scripted session produces, byte for byte. The server's own test validates this very file,
    /// so the recorder and the server cannot drift apart on the format.
    func testTheGoldenArchiveIsWhatTheRecorderProduces() throws {
        let s = session()
        clock.advance(seconds: 5)
        _ = try s.record(observation(.click, at: clock, title: "New Note"), owner: testOwner)
        clock.advance(seconds: 4)
        _ = try s.record(observation(.type, at: clock, role: "AXTextArea", title: "Note body", text: "Quarterly report"), owner: testOwner)
        clock.advance(seconds: 3)
        try s.appendTranscript("now I make a new note", owner: testOwner)
        clock.advance(seconds: 3)
        _ = try s.record(
            observation(.type, at: clock, role: "AXTextField", title: "Password", text: "hunter2", subrole: "AXSecureTextField"),
            owner: testOwner)
        clock.advance(seconds: 3)
        _ = try s.record(observation(.click, at: clock, title: "Save"), owner: testOwner)
        clock.advance(seconds: 2)
        let finished = try s.finish(owner: testOwner, requestId: "fin_1")
        let url = repositoryRoot.appendingPathComponent("tests/contract/fixtures/recording/archive_from_recorder.json")
        if ProcessInfo.processInfo.environment["QUANSIO_UPDATE_GOLDEN"] == "1" {
            try finished.data.write(to: url)
        }
        let golden = try Data(contentsOf: url)
        XCTAssertEqual(
            String(decoding: finished.data, as: UTF8.self), String(decoding: golden, as: UTF8.self),
            "regenerate with QUANSIO_UPDATE_GOLDEN=1 only for an intended change of the archive format")
    }
}
