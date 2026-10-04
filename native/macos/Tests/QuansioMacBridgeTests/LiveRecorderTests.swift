import ApplicationServices
import XCTest

@testable import QuansioMacBridge

/// The recorder against the live system (ADP-003): the real event tap, the real accessibility lookup of a
/// control, the real screen capture.
///
/// `QUANSIO_TEST_MACOS_RECORDER=1` enables them; each then needs the permission it exercises (Input
/// Monitoring for the tap, Accessibility for the lookup, Screen Recording for capture) and says
/// `BLOCKED_EXTERNAL` for the one that is missing. They post no input and read nothing the person did: the
/// tap is created and torn down, the lookup reads the menu bar's first item, and the capture is judged and
/// discarded.
final class LiveRecorderTests: XCTestCase {
    override func setUpWithError() throws {
        guard ProcessInfo.processInfo.environment["QUANSIO_TEST_MACOS_RECORDER"] == "1" else {
            throw XCTSkip("BLOCKED_EXTERNAL: QUANSIO_TEST_MACOS_RECORDER=1 is required for the live macOS recorder boundary")
        }
    }

    func testTheLiveObserverStartsOnlyWithItsPermissionsAndStopsCleanly() throws {
        let observer = LiveEventObserver()
        do {
            try observer.start { _ in }
        } catch RecorderError.permissionRequired(let permission, let status) {
            let granted = permissionStatus()
            // A refusal is typed and agrees with what the system says about the permission it names.
            switch permission {
            case "accessibility": XCTAssertNotEqual(granted.accessibility, .granted)
            case "input_monitoring": XCTAssertNotEqual(granted.inputMonitoring, .granted)
            default: XCTFail("unexpected permission \(permission)")
            }
            throw XCTSkip("BLOCKED_EXTERNAL: \(permission) is \(status) for this process; the live event tap cannot be created")
        }
        observer.stop()
        observer.stop()  // idempotent
    }

    func testTheAccessibilityLookupNamesARealControlByItsAttributes() throws {
        guard automationTrusted() else {
            throw XCTSkip("BLOCKED_EXTERNAL: Accessibility is not granted to this process")
        }
        // Whatever is at the top-left of the screen (the Apple menu in a signed-in session, the login window
        // on a locked one): the lookup must name it by accessibility attributes, which is what a recorded
        // click is built from.
        guard let control = controlAt(x: 16, y: 6) else {
            throw XCTSkip("BLOCKED_EXTERNAL: no window-server session answers an accessibility lookup here")
        }
        XCTAssertNotNil(control.attributes.role, "\(control)")
        XCTAssertGreaterThan(control.pid, 0)
        XCTAssertNotNil(
            control.attributes.title ?? control.attributes.descriptionText ?? control.attributes.identifier
                ?? control.attributes.subrole, "\(control)")
    }

    func testTheRealScreenIsCapturedJudgedByItsTextAndBounded() throws {
        guard permissionStatus().screenRecording == .granted else {
            throw XCTSkip("BLOCKED_EXTERNAL: Screen Recording is not granted to this process")
        }
        var limits = RecordingLimits(screenshotMinIntervalMs: 0)
        limits.screenshotMaxDimensionPx = 800
        let sampler = ScreenshotSampler(limits: limits, recognizer: VisionTextRecognizer()) { try liveScreenCapture() }
        let outcome = sampler.sample(at: Date())
        if outcome == .unavailable {
            throw XCTSkip("BLOCKED_EXTERNAL: the display could not be captured (a locked screen or no display session)")
        }
        XCTAssertTrue([.kept, .withheld, .tooLarge].contains(outcome), "\(outcome)")
        for shot in sampler.kept {
            XCTAssertLessThanOrEqual(max(shot.width, shot.height), 800)
            XCTAssertLessThanOrEqual(shot.png.count, limits.screenshotMaxBytes)
            XCTAssertTrue(scanText(shot.screenText).isEmpty, "what is kept holds nothing redaction removes")
        }
    }
}
