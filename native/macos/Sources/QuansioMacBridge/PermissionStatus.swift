// OS permission status and foreground-control state (ADP-002, DOMAIN.md 8.6.1 E).
//
// This bridge only reports what the system says. Whether a missing permission parks a run, and what the
// person is told, is the Rust runtime's decision; opening System Settings is the desktop's, on the
// person's click. Nothing here opens a pane or prompts for a permission.
//
// macOS exposes a tri-state for some permissions and a boolean for others, and the mapping is honest
// about it: where the system cannot distinguish "refused" from "never asked" (Accessibility and Screen
// Recording report only whether access exists) the answer is `denied`, which sends the person to the same
// Settings pane either way. `not_determined` is reported only where the system really says so.

import ApplicationServices
import AVFoundation
import CoreGraphics
import Foundation
import IOKit.hid

public enum PermissionState: String, Codable, Sendable, Equatable {
    case granted
    case denied
    case notDetermined = "not_determined"
    case restricted
}

/// The four permissions the bridge reports, in the order the wire form lists them.
public struct PermissionStatus: Codable, Sendable, Equatable {
    public var accessibility: PermissionState
    public var screenRecording: PermissionState
    public var inputMonitoring: PermissionState
    public var microphone: PermissionState

    public init(
        accessibility: PermissionState,
        screenRecording: PermissionState,
        inputMonitoring: PermissionState,
        microphone: PermissionState
    ) {
        self.accessibility = accessibility
        self.screenRecording = screenRecording
        self.inputMonitoring = inputMonitoring
        self.microphone = microphone
    }

    enum CodingKeys: String, CodingKey {
        case accessibility
        case screenRecording = "screen_recording"
        case inputMonitoring = "input_monitoring"
        case microphone
    }
}

/// Each probe on its own, so a test can substitute one and the live probes stay in one place.
public struct PermissionProbes: Sendable {
    public var accessibility: @Sendable () -> PermissionState
    public var screenRecording: @Sendable () -> PermissionState
    public var inputMonitoring: @Sendable () -> PermissionState
    public var microphone: @Sendable () -> PermissionState

    public init(
        accessibility: @escaping @Sendable () -> PermissionState,
        screenRecording: @escaping @Sendable () -> PermissionState,
        inputMonitoring: @escaping @Sendable () -> PermissionState,
        microphone: @escaping @Sendable () -> PermissionState
    ) {
        self.accessibility = accessibility
        self.screenRecording = screenRecording
        self.inputMonitoring = inputMonitoring
        self.microphone = microphone
    }

    /// What the running system says.
    public static let live = PermissionProbes(
        accessibility: { AXIsProcessTrusted() ? .granted : .denied },
        screenRecording: { CGPreflightScreenCaptureAccess() ? .granted : .denied },
        inputMonitoring: { mapInputMonitoring(IOHIDCheckAccess(kIOHIDRequestTypeListenEvent)) },
        microphone: { mapMicrophone(AVCaptureDevice.authorizationStatus(for: .audio)) }
    )
}

/// `IOHIDAccessType` to the typed state. An unknown answer is "not determined", never "granted".
public func mapInputMonitoring(_ access: IOHIDAccessType) -> PermissionState {
    switch access {
    case kIOHIDAccessTypeGranted: return .granted
    case kIOHIDAccessTypeDenied: return .denied
    default: return .notDetermined
    }
}

/// `AVAuthorizationStatus` to the typed state.
public func mapMicrophone(_ status: AVAuthorizationStatus) -> PermissionState {
    switch status {
    case .authorized: return .granted
    case .denied: return .denied
    case .restricted: return .restricted
    case .notDetermined: return .notDetermined
    @unknown default: return .notDetermined
    }
}

/// What the system says about the four permissions right now.
public func permissionStatus(_ probes: PermissionProbes = .live) -> PermissionStatus {
    PermissionStatus(
        accessibility: probes.accessibility(),
        screenRecording: probes.screenRecording(),
        inputMonitoring: probes.inputMonitoring(),
        microphone: probes.microphone()
    )
}

/// The wire form of a status, with sorted keys.
public func permissionStatusJSON(_ status: PermissionStatus) throws -> Data {
    let encoder = JSONEncoder()
    encoder.outputFormatting = [.sortedKeys]
    return try encoder.encode(status)
}

// MARK: foreground control

/// Whether the person's own input has taken the foreground back from the agent.
public enum ForegroundControl: String, Codable, Sendable, Equatable {
    case held
    case lapsed
}

/// What the system says about input, so the monitor can be driven without touching the machine.
public protocol InputClock: Sendable {
    /// Monotonic seconds.
    func now() -> TimeInterval
    /// Seconds since the last input event of any kind from the system input stream.
    func secondsSinceLastInput() -> TimeInterval
}

/// The running system's clock and input stream.
public struct LiveInputClock: InputClock {
    public init() {}

    public func now() -> TimeInterval { ProcessInfo.processInfo.systemUptime }

    public func secondsSinceLastInput() -> TimeInterval {
        let kinds: [CGEventType] = [
            .mouseMoved, .leftMouseDown, .rightMouseDown, .otherMouseDown,
            .leftMouseDragged, .keyDown, .scrollWheel, .flagsChanged,
        ]
        return kinds
            .map { CGEventSource.secondsSinceLastEventType(.hidSystemState, eventType: $0) }
            .min() ?? .infinity
    }
}

/// Tells "the agent holds the foreground" from "the person is using the machine".
///
/// The agent stamps the time right after it posts input. Input newer than that stamp is the person's,
/// and the grant is lapsed for as long as they keep going: once they have been quiet for `pauseSeconds`
/// it is held again, which is what lets "pause input" resolve by the person pausing.
public final class ForegroundControlMonitor: @unchecked Sendable {
    private let clock: InputClock
    private let pauseSeconds: TimeInterval
    private let tolerance: TimeInterval
    private let lock = NSLock()
    private var lastAgentInput: TimeInterval?

    /// The monitor the receipted input functions stamp, and the foreground-control answer reads.
    public static let shared = ForegroundControlMonitor()

    public init(clock: InputClock = LiveInputClock(), pauseSeconds: TimeInterval = 1.5, tolerance: TimeInterval = 0.05) {
        self.clock = clock
        self.pauseSeconds = pauseSeconds
        self.tolerance = tolerance
    }

    /// Call right after the agent posts input.
    public func recordAgentInput() {
        lock.lock()
        lastAgentInput = clock.now()
        lock.unlock()
    }

    /// The current state.
    public func state() -> ForegroundControl {
        lock.lock()
        let stamp = lastAgentInput
        lock.unlock()
        let idle = clock.secondsSinceLastInput()
        let sinceAgent = stamp.map { clock.now() - $0 } ?? .infinity
        // Input newer than the agent's own last input is the person's.
        let personInputAfterAgent = idle < sinceAgent - tolerance
        return personInputAfterAgent && idle < pauseSeconds ? .lapsed : .held
    }
}
