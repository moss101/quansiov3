import XCTest
import CoreGraphics

@testable import QuansioMacBridge

/// Background-operation measurement (ADP-002, DOMAIN.md 8.6.1 C).
///
/// The logic is driven with a scripted probe, so every kind of violation is exercised without moving the
/// pointer or pressing a key on the machine running the suite. The live tests, behind the same switch as
/// the rest of the live boundary, ask the real system and move the pointer by one point and put it back:
/// that is the only way to know the measurement sees a real cursor move, and it needs no permission.
final class BackgroundInvariantTests: XCTestCase {
    /// A probe that plays back readings; the body of a measurement advances it.
    final class ScriptedProbe: SystemProbe, @unchecked Sendable {
        var fronts: [FrontmostApp?]
        var cursors: [CGPoint?]
        var idles: [TimeInterval]
        var clock: [TimeInterval]

        init(fronts: [FrontmostApp?], cursors: [CGPoint?], idles: [TimeInterval], clock: [TimeInterval]) {
            self.fronts = fronts
            self.cursors = cursors
            self.idles = idles
            self.clock = clock
        }

        func frontmost() -> FrontmostApp? { fronts.removeFirst() }
        func cursorLocation() -> CGPoint? { cursors.removeFirst() }
        func secondsSinceLastInput() -> TimeInterval { idles.removeFirst() }
        func now() -> TimeInterval { clock.removeFirst() }
    }

    private let editor = FrontmostApp(bundleId: "com.apple.TextEdit", pid: 10)
    private let browser = FrontmostApp(bundleId: "com.apple.Safari", pid: 20)
    private let origin = CGPoint(x: 100, y: 200)

    func testIdleTimeThatGrowsByExactlyTheElapsedTimeMeansNoInput() {
        // 5 s idle before, 0.2 s window, 5.2 s idle after: nothing happened.
        XCTAssertFalse(physicalInputDetected(idleBefore: 5, idleAfter: 5.2, elapsed: 0.2))
        // Granularity slack is absorbed.
        XCTAssertFalse(physicalInputDetected(idleBefore: 5, idleAfter: 5.17, elapsed: 0.2))
        // An event 0.05 s into the window leaves the idle time at 0.15 s.
        XCTAssertTrue(physicalInputDetected(idleBefore: 5, idleAfter: 0.15, elapsed: 0.2))
        // Even a small event: idle reset to 0.01 s at the end of a 1 s window.
        XCTAssertTrue(physicalInputDetected(idleBefore: 30, idleAfter: 0.01, elapsed: 1))
        // The very first event of a session (idle was infinite) is still caught, and no event is not.
        XCTAssertTrue(physicalInputDetected(idleBefore: .infinity, idleAfter: 3, elapsed: 4))
        XCTAssertFalse(physicalInputDetected(idleBefore: .infinity, idleAfter: .infinity, elapsed: 4))
    }

    func testACleanWindowMeasuresClean() throws {
        let probe = ScriptedProbe(
            fronts: [editor, editor], cursors: [origin, origin], idles: [5, 5.2], clock: [100, 100.2]
        )
        let (value, invariants) = measureBackground(probe: probe) { 7 }
        XCTAssertEqual(value, 7)
        let measured = try XCTUnwrap(invariants)
        XCTAssertFalse(measured.physicalInput)
        XCTAssertFalse(measured.movedSystemCursor)
        XCTAssertFalse(measured.frontmostChanged)
        XCTAssertFalse(measured.violated)
    }

    func testEachViolationIsSeenOnItsOwn() throws {
        // Physical input.
        var probe = ScriptedProbe(fronts: [editor, editor], cursors: [origin, origin], idles: [5, 0.1], clock: [0, 0.3])
        var invariants = try XCTUnwrap(measureBackground(probe: probe) { () }.invariants)
        XCTAssertTrue(invariants.physicalInput)
        XCTAssertFalse(invariants.movedSystemCursor)
        XCTAssertFalse(invariants.frontmostChanged)

        // The pointer moved.
        probe = ScriptedProbe(
            fronts: [editor, editor], cursors: [origin, CGPoint(x: 101, y: 200)], idles: [5, 5.3], clock: [0, 0.3]
        )
        invariants = try XCTUnwrap(measureBackground(probe: probe) { () }.invariants)
        XCTAssertTrue(invariants.movedSystemCursor)
        XCTAssertFalse(invariants.physicalInput)
        XCTAssertFalse(invariants.frontmostChanged)

        // The frontmost application changed.
        probe = ScriptedProbe(fronts: [editor, browser], cursors: [origin, origin], idles: [5, 5.3], clock: [0, 0.3])
        invariants = try XCTUnwrap(measureBackground(probe: probe) { () }.invariants)
        XCTAssertTrue(invariants.frontmostChanged)
        XCTAssertEqual(invariants.frontmostBefore, editor)
        XCTAssertEqual(invariants.frontmostAfter, browser)
        XCTAssertFalse(invariants.movedSystemCursor)
        XCTAssertFalse(invariants.physicalInput)

        // The same application, restarted under another pid.
        probe = ScriptedProbe(
            fronts: [editor, FrontmostApp(bundleId: editor.bundleId, pid: 11)],
            cursors: [origin, origin], idles: [5, 5.3], clock: [0, 0.3]
        )
        invariants = try XCTUnwrap(measureBackground(probe: probe) { () }.invariants)
        XCTAssertTrue(invariants.frontmostChanged)
    }

    func testAMeasurementThatCannotBeMadeIsReportedAsUnmeasuredNeverAsClean() throws {
        // No frontmost application readable afterwards.
        var probe = ScriptedProbe(fronts: [editor, nil], cursors: [origin, origin], idles: [5, 5.3], clock: [0, 0.3])
        XCTAssertNil(measureBackground(probe: probe) { () }.invariants)
        // No pointer to read (no window-server session).
        probe = ScriptedProbe(fronts: [editor, editor], cursors: [nil, nil], idles: [5, 5.3], clock: [0, 0.3])
        XCTAssertNil(measureBackground(probe: probe) { () }.invariants)
        // And an unmeasured background action is lowered to unknown by the receipt rules.
        let receipt = ActionReceipt.applied(.processEvent, .background).measured(nil)
        XCTAssertNil(receipt.effectVerified)
    }

    func testABodyThatThrowsPropagatesAndMeasuresNothing() {
        struct Boom: Error {}
        let probe = ScriptedProbe(fronts: [editor], cursors: [origin], idles: [5], clock: [0])
        XCTAssertThrowsError(try measureBackground(probe: probe) { throw Boom() })
    }

    func testTypedTextVerificationNeverRoundsUnknownToAnAnswer() {
        // Unreadable afterwards: unknown.
        XCTAssertNil(typedTextVerification(typed: "abc", valueBefore: "x", valueAfter: nil))
        XCTAssertNil(typedTextVerification(typed: "abc", valueBefore: nil, valueAfter: nil))
        // Nothing was typed: nothing to verify.
        XCTAssertNil(typedTextVerification(typed: "", valueBefore: "x", valueAfter: "x"))
        // Readable and the text is there where it was not: verified.
        XCTAssertEqual(typedTextVerification(typed: "abc", valueBefore: "x", valueAfter: "xabc"), true)
        XCTAssertEqual(typedTextVerification(typed: "abc", valueBefore: nil, valueAfter: "abc"), true)
        // Readable and the text is not there: not applied.
        XCTAssertEqual(typedTextVerification(typed: "abc", valueBefore: "x", valueAfter: "x"), false)
        // The text was already there and did not grow: not proof.
        XCTAssertEqual(typedTextVerification(typed: "abc", valueBefore: "abc", valueAfter: "abc"), false)
        XCTAssertEqual(typedTextVerification(typed: "abc", valueBefore: "abc", valueAfter: "abcabc"), true)
    }

    // MARK: live system

    private func requireLiveBoundary() throws {
        guard ProcessInfo.processInfo.environment["QUANSIO_TEST_MACOS_COMPUTER_USE"] == "1" else {
            throw XCTSkip(
                "BLOCKED_EXTERNAL: QUANSIO_TEST_MACOS_COMPUTER_USE=1 is required for the live macOS computer-use boundary"
            )
        }
    }

    func testTheLiveProbeSeesARealCursorMoveAndNothingElse() throws {
        try requireLiveBoundary()
        guard let start = CGEvent(source: nil)?.location else {
            throw XCTSkip("BLOCKED_EXTERNAL: there is no window-server session to read the pointer from")
        }
        defer { CGWarpMouseCursorPosition(start) }
        // A no-op window does not move the pointer or change the frontmost application. (A person using
        // the machine during the window can fail this; the suite is meant to run on an idle one.)
        let quiet = measureBackground { () }
        if let invariants = quiet.invariants {
            XCTAssertFalse(invariants.movedSystemCursor)
            XCTAssertFalse(invariants.frontmostChanged)
        }
        // Moving the pointer by one point inside the window is seen as a cursor move.
        let moved = measureBackground {
            CGWarpMouseCursorPosition(CGPoint(x: start.x + 1, y: start.y))
        }
        let invariants = try XCTUnwrap(moved.invariants, "the live probe could not measure")
        XCTAssertTrue(invariants.movedSystemCursor, "a real one-point cursor move was not seen")
        XCTAssertTrue(invariants.violated)
        // The receipt for an action that did that is not verified, whatever the action reported.
        let receipt = ActionReceipt.applied(.processEvent, .background).measured(invariants)
        XCTAssertEqual(receipt.outcome, .invariantViolated)
        XCTAssertEqual(receipt.effectVerified, false)
    }

    func testAClipboardWriteIsReceiptedMeasuredAndVerifiedByReadBack() throws {
        try requireLiveBoundary()
        let app = try frontmostApp()
        let previous = clipboardText()
        defer { try? setClipboardText(previous, expectedBundleId: app.bundleId) }
        let marker = "quansio-receipt-test-\(UUID().uuidString)"
        let receipt = try receiptedClipboardWrite(marker, expectedBundleId: app.bundleId)
        XCTAssertEqual(clipboardText(), marker)
        XCTAssertTrue(receipt.dispatched)
        XCTAssertEqual(receipt.method, .pasteboard)
        XCTAssertEqual(receipt.executionMethod, .background)
        if let invariants = receipt.invariants {
            // Measured: verified unless the machine was being used during the write.
            if invariants.violated {
                XCTAssertEqual(receipt.outcome, .invariantViolated)
                XCTAssertEqual(receipt.effectVerified, false)
            } else {
                XCTAssertEqual(receipt.effectVerified, true)
                XCTAssertEqual(receipt.outcome, .applied)
            }
        } else {
            // Could not be measured (no session): the verification is unknown, not true.
            XCTAssertNil(receipt.effectVerified)
            XCTAssertEqual(receipt.outcome, .unverified)
        }
        // A read is observed, with no verdict.
        let read = try receiptedClipboardRead(expectedBundleId: app.bundleId)
        XCTAssertEqual(read.text, marker)
        XCTAssertNil(read.receipt.effectVerified)
        XCTAssertTrue([.observed, .invariantViolated].contains(read.receipt.outcome))
    }

    func testAnUnauthorizedForegroundIsRefusedBeforeAnythingIsWritten() throws {
        try requireLiveBoundary()
        let before = clipboardText()
        XCTAssertThrowsError(
            try receiptedClipboardWrite("must-not-be-written", expectedBundleId: "com.quansio.not-the-foreground-app")
        )
        XCTAssertEqual(clipboardText(), before)
    }
}
