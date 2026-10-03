import XCTest
import AVFoundation
import IOKit.hid

@testable import QuansioMacBridge

/// Permission status and foreground control (ADP-002, DOMAIN.md 8.6.1 E).
///
/// Reading a permission's status is a read, so these tests never prompt and never open System Settings.
/// The pure mappings and the foreground-control logic run everywhere; the live reads are behind the
/// same switch as the rest of the live boundary.
final class PermissionStatusTests: XCTestCase {
    func testTheStatusIsExactlyFourPermissionsInSnakeCase() throws {
        let status = PermissionStatus(
            accessibility: .granted,
            screenRecording: .denied,
            inputMonitoring: .notDetermined,
            microphone: .restricted
        )
        let object = try XCTUnwrap(
            try JSONSerialization.jsonObject(with: try permissionStatusJSON(status)) as? [String: String]
        )
        XCTAssertEqual(
            object,
            [
                "accessibility": "granted",
                "screen_recording": "denied",
                "input_monitoring": "not_determined",
                "microphone": "restricted",
            ]
        )
        // And it decodes back.
        let decoded = try JSONDecoder().decode(PermissionStatus.self, from: try permissionStatusJSON(status))
        XCTAssertEqual(decoded, status)
    }

    func testInputMonitoringNeverReportsGrantedForAnAnswerItDoesNotUnderstand() {
        XCTAssertEqual(mapInputMonitoring(kIOHIDAccessTypeGranted), .granted)
        XCTAssertEqual(mapInputMonitoring(kIOHIDAccessTypeDenied), .denied)
        XCTAssertEqual(mapInputMonitoring(kIOHIDAccessTypeUnknown), .notDetermined)
        XCTAssertEqual(mapInputMonitoring(IOHIDAccessType(rawValue: 99)), .notDetermined)
    }

    func testMicrophoneMapsEveryAuthorizationStatus() {
        XCTAssertEqual(mapMicrophone(.authorized), .granted)
        XCTAssertEqual(mapMicrophone(.denied), .denied)
        XCTAssertEqual(mapMicrophone(.restricted), .restricted)
        XCTAssertEqual(mapMicrophone(.notDetermined), .notDetermined)
    }

    func testAStatusIsBuiltFromEachProbeIndependently() {
        let probes = PermissionProbes(
            accessibility: { .granted },
            screenRecording: { .denied },
            inputMonitoring: { .notDetermined },
            microphone: { .restricted }
        )
        XCTAssertEqual(
            permissionStatus(probes),
            PermissionStatus(accessibility: .granted, screenRecording: .denied, inputMonitoring: .notDetermined, microphone: .restricted)
        )
    }

    // MARK: foreground control

    /// A clock the test advances; `idle` is how long ago the last input of any kind was.
    final class FakeClock: InputClock, @unchecked Sendable {
        var time: TimeInterval = 1000
        var lastInputAt: TimeInterval = 0

        func now() -> TimeInterval { time }
        func secondsSinceLastInput() -> TimeInterval { time - lastInputAt }
    }

    func testTheAgentsOwnInputDoesNotLapseItsGrant() {
        let clock = FakeClock()
        let monitor = ForegroundControlMonitor(clock: clock)
        clock.time = 1010
        // The agent posts input; the system stamps the event a hair before the agent records it.
        clock.lastInputAt = 1010
        clock.time = 1010.01
        monitor.recordAgentInput()
        XCTAssertEqual(monitor.state(), .held)
        clock.time = 1011
        XCTAssertEqual(monitor.state(), .held)
    }

    func testAPersonsInputAfterTheAgentsLapsesTheGrantAndPausingRestoresIt() {
        let clock = FakeClock()
        let monitor = ForegroundControlMonitor(clock: clock, pauseSeconds: 1.5)
        clock.lastInputAt = 1010
        clock.time = 1010.01
        monitor.recordAgentInput()
        XCTAssertEqual(monitor.state(), .held)
        // The person moves the mouse two seconds later.
        clock.time = 1012.2
        clock.lastInputAt = 1012.1
        XCTAssertEqual(monitor.state(), .lapsed)
        // They keep going.
        clock.time = 1013
        clock.lastInputAt = 1012.9
        XCTAssertEqual(monitor.state(), .lapsed)
        // They stop: after the pause the grant is back, with no action by the agent.
        clock.time = 1015
        XCTAssertEqual(monitor.state(), .held)
    }

    func testAPersonActiveBeforeTheAgentHasInputAnythingIsAlreadyInTheWay() {
        let clock = FakeClock()
        let monitor = ForegroundControlMonitor(clock: clock, pauseSeconds: 1.5)
        clock.time = 2000
        clock.lastInputAt = 1999.7
        XCTAssertEqual(monitor.state(), .lapsed)
        clock.time = 2005
        XCTAssertEqual(monitor.state(), .held)
    }

    func testTheWireWordsAreHeldAndLapsed() {
        XCTAssertEqual(ForegroundControl.held.rawValue, "held")
        XCTAssertEqual(ForegroundControl.lapsed.rawValue, "lapsed")
    }

    // MARK: live system

    private func requireLiveBoundary() throws {
        guard ProcessInfo.processInfo.environment["QUANSIO_TEST_MACOS_COMPUTER_USE"] == "1" else {
            throw XCTSkip(
                "BLOCKED_EXTERNAL: QUANSIO_TEST_MACOS_COMPUTER_USE=1 is required for the live macOS computer-use boundary"
            )
        }
    }

    func testTheLiveStatusAgreesWithTheSystemsOwnAnswers() throws {
        try requireLiveBoundary()
        let status = permissionStatus()
        // Accessibility and Screen Recording are booleans on this system, so they are granted or denied.
        XCTAssertEqual(status.accessibility, automationTrusted() ? .granted : .denied)
        XCTAssertEqual(status.screenRecording, CGPreflightScreenCaptureAccess() ? .granted : .denied)
        XCTAssertEqual(status.microphone, mapMicrophone(AVCaptureDevice.authorizationStatus(for: .audio)))
        XCTAssertEqual(status.inputMonitoring, mapInputMonitoring(IOHIDCheckAccess(kIOHIDRequestTypeListenEvent)))
        // A process that is not trusted for Accessibility cannot read a tree, which is what the status
        // says it cannot do.
        if status.accessibility != .granted {
            let app = try frontmostApp()
            XCTAssertThrowsError(try boundedTree(pid: app.pid, maxNodes: 1, maxDepth: 0)) { error in
                XCTAssertEqual(error as? ComputerUseError, .automationNotTrusted)
            }
        }
    }

    func testTheLiveForegroundControlAnswersWithoutTouchingTheMachine() throws {
        try requireLiveBoundary()
        let state = ForegroundControlMonitor().state()
        XCTAssertTrue([ForegroundControl.held, .lapsed].contains(state))
    }
}
