import XCTest

@testable import QuansioMacBridge

/// The real secure-event-input state (ADP-003, DOMAIN.md 11.7 C): this process takes secure input the way a
/// password field does, and the recorder's live probe sees it. No permission is needed to take or read it.
final class SecureInputTests: XCTestCase {
    func testTheLiveProbeSeesRealSecureEventInputAndTheSessionPausesOnIt() throws {
        let probe = LiveSecureInputProbe()
        guard !probe.isSecureInputActive() else {
            throw XCTSkip("BLOCKED_EXTERNAL: another process already holds secure input on this host (a password prompt)")
        }
        let clock = ManualClock()
        let session = RecorderSession(
            recordingId: testRecording, owner: testOwner, limits: RecordingLimits(), clock: clock, secureInput: probe)
        clock.advance(seconds: 1)
        XCTAssertEqual(try session.record(observation(.click, at: clock, title: "Before"), owner: testOwner), .recorded(seq: 1))

        let release = SecureInputHold.acquire()
        XCTAssertTrue(probe.isSecureInputActive(), "the system says secure input is on")
        clock.advance(seconds: 1)
        XCTAssertEqual(
            try session.record(observation(.type, at: clock, role: "AXTextField", text: "swordfish"), owner: testOwner),
            .paused(reason: "secure_input"))
        release()
        XCTAssertFalse(probe.isSecureInputActive(), "and off again once it is released")
        clock.advance(seconds: 1)
        XCTAssertEqual(try session.record(observation(.click, at: clock, title: "After"), owner: testOwner), .recorded(seq: 2))
        clock.advance(seconds: 1)
        let archive = try session.finish(owner: testOwner, requestId: "fin_1")
        XCTAssertFalse(String(decoding: archive.data, as: UTF8.self).contains("swordfish"))
        let document = try jsonObject(archive.data)
        XCTAssertEqual((document["paused"] as? [[String: Any]])?.first?["reason"] as? String, "secure_input")
    }
}
