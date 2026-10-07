// The recorder's request handler (ADP-003, DOMAIN.md 11.7 A, B): what each control request does to the one
// recording session this process may hold.
//
// Pure of sockets so it is tested without one; `RecorderControlServer` supplies the authenticated channel.
//
//  * `start` refuses until every permission the recording needs is granted, then begins observing;
//  * every request that touches the session carries the owner it was made under, and any other owner ends
//    the session and discards what it held;
//  * `start`, `finish` and `cancel` are idempotent by request id and a fingerprint of their arguments: the
//    same pair replays the first answer, the same id with other arguments is refused;
//  * there is no request that finishes a recording except `finish`, and no request that injects an action:
//    what is in an archive was observed by this process.

import CryptoKit
import Foundation

/// Everything the controller needs from the platform, replaceable for a test of the logic alone.
public struct RecorderEnvironment: @unchecked Sendable {
    public var clock: RecorderClock
    public var secureInput: SecureInputProbe
    public var recognizer: TextRecognizer
    public var capture: @Sendable () throws -> Data
    public var permissions: PermissionProbes
    public var makeSource: @Sendable () -> ObservationSource?

    public init(
        clock: RecorderClock = SystemRecorderClock(), secureInput: SecureInputProbe = LiveSecureInputProbe(),
        recognizer: TextRecognizer = VisionTextRecognizer(),
        capture: @escaping @Sendable () throws -> Data = { try liveScreenCapture() },
        permissions: PermissionProbes = .live,
        makeSource: @escaping @Sendable () -> ObservationSource? = { nil }
    ) {
        self.clock = clock
        self.secureInput = secureInput
        self.recognizer = recognizer
        self.capture = capture
        self.permissions = permissions
        self.makeSource = makeSource
    }
}

private func canonicalJSON(_ value: Any) -> String {
    guard JSONSerialization.isValidJSONObject(value),
        let data = try? JSONSerialization.data(withJSONObject: value, options: [.sortedKeys, .withoutEscapingSlashes]),
        let text = String(data: data, encoding: .utf8)
    else { return "\(value)" }
    return text
}

/// Handles control requests for one recording session.
public final class RecorderController: @unchecked Sendable {
    private let environment: RecorderEnvironment
    private let lock = NSLock()
    private var session: RecorderSession?
    private var source: ObservationSource?
    private var timer: DispatchSourceTimer?
    private var replies: [String: (fingerprint: String, response: [String: Any])] = [:]

    public init(environment: RecorderEnvironment) {
        self.environment = environment
    }

    /// Stop observing and discard everything: the control channel is gone, so nobody can finish this recording.
    public func shutdown() {
        lock.lock()
        defer { lock.unlock() }
        endLocked()
        if let session, session.currentState == .recording {
            try? session.cancel(owner: session.owner)
        }
    }

    private func endLocked() {
        timer?.cancel()
        timer = nil
        source?.stop()
        source = nil
    }

    // MARK: dispatch

    /// Handle one request object and answer it. Never throws: a refusal is an `ok: false` answer.
    public func handle(_ request: [String: Any]) -> [String: Any] {
        let id = request["id"] as? String
        var response: [String: Any]
        guard let op = request["op"] as? String else {
            return reply(id, error: "invalid_request", detail: "a request needs an op")
        }
        switch op {
        case "permissions":
            response = ["ok": true, "permissions": permissionJSON(permissionStatus(environment.permissions))]
        case "start", "finish", "cancel":
            response = idempotent(op: op, id: id, request: request)
        case "status":
            response = guarded(request) { session, owner in
                let status = try session.status(owner: owner)
                return [
                    "state": status.state, "actions": status.actions, "screenshots": status.screenshots,
                    "screenshots_withheld": status.screenshotsWithheld,
                    "transcript_segments": status.transcriptSegments, "paused": status.pausedReason as Any,
                    "dropped_for_limit": status.droppedForLimit, "counts": status.counts,
                ]
            }
        case "transcript":
            response = guarded(request) { session, owner in
                guard let text = request["text"] as? String else {
                    throw RecorderError.invalidRequest("transcript needs text")
                }
                try session.appendTranscript(text, owner: owner)
                return [:]
            }
        default:
            response = ["ok": false, "error": "unknown_op", "detail": op]
        }
        if let id { response["id"] = id }
        return response
    }

    private func reply(_ id: String?, error: String, detail: String) -> [String: Any] {
        var out: [String: Any] = ["ok": false, "error": error, "detail": detail]
        if let id { out["id"] = id }
        return out
    }

    private func idempotent(op: String, id: String?, request: [String: Any]) -> [String: Any] {
        guard let id, !id.isEmpty else {
            return ["ok": false, "error": "invalid_request", "detail": "\(op) needs an id"]
        }
        var args = request
        args.removeValue(forKey: "id")
        let fingerprint = SHA256.hash(data: Data(canonicalJSON(args).utf8)).map { String(format: "%02x", $0) }.joined()
        lock.lock()
        if let seen = replies[id] {
            lock.unlock()
            if seen.fingerprint != fingerprint {
                return ["ok": false, "error": "idempotency_mismatch", "detail": "id \(id) was used with other arguments"]
            }
            return seen.response
        }
        lock.unlock()
        let response: [String: Any]
        switch op {
        case "start": response = start(request)
        case "finish":
            response = guarded(request) { session, owner in
                guard let requestId = request["request_id"] as? String, !requestId.isEmpty else {
                    throw RecorderError.invalidRequest("finish needs a request_id")
                }
                let archive = try session.finish(owner: owner, requestId: requestId)
                self.lock.lock()
                self.endLocked()
                self.lock.unlock()
                return [
                    "archive_base64": archive.data.base64EncodedString(), "sha256": archive.sha256,
                    "size": archive.data.count, "actions": archive.actions, "screenshots": archive.screenshots,
                ]
            }
        default:
            response = guarded(request) { session, owner in
                try session.cancel(owner: owner)
                self.lock.lock()
                self.endLocked()
                self.lock.unlock()
                return [:]
            }
        }
        // A refusal is not remembered, so a retry after the cause is fixed (a granted permission) can succeed.
        if response["ok"] as? Bool == true {
            lock.lock()
            replies[id] = (fingerprint, response)
            lock.unlock()
        }
        return response
    }

    // MARK: start

    private func start(_ request: [String: Any]) -> [String: Any] {
        guard let recordingId = request["recording_id"] as? String, !recordingId.isEmpty,
            let owner = RecordingOwner(wire: request["owner"]),
            let durationMs = request["max_duration_ms"] as? Int,
            let limits = RecordingLimits(wire: request["limits"], maxDurationMs: durationMs)
        else {
            return ["ok": false, "error": "invalid_request", "detail": "start needs recording_id, owner, limits and max_duration_ms"]
        }
        let narration = request["narration"] as? Bool ?? false
        lock.lock()
        defer { lock.unlock() }
        if let existing = session, existing.currentState == .recording {
            return ["ok": false, "error": "already_recording", "detail": "this recorder holds a recording in progress"]
        }
        // Nothing is captured until every permission the recording needs is granted.
        let status = permissionStatus(environment.permissions)
        var needed: [(String, PermissionState)] = [
            ("accessibility", status.accessibility), ("input_monitoring", status.inputMonitoring),
        ]
        if limits.screenshotsEnabled { needed.append(("screen_recording", status.screenRecording)) }
        if narration { needed.append(("microphone", status.microphone)) }
        if let missing = needed.first(where: { $0.1 != .granted }) {
            return [
                "ok": false, "error": "permission_required", "permission": missing.0, "status": missing.1.rawValue,
                "permissions": permissionJSON(status),
            ]
        }
        let sampler: ScreenshotSampler? =
            limits.screenshotsEnabled
            ? ScreenshotSampler(limits: limits, recognizer: environment.recognizer, capture: environment.capture) : nil
        let fresh = RecorderSession(
            recordingId: recordingId, owner: owner, limits: limits, clock: environment.clock,
            secureInput: environment.secureInput, sampler: sampler)
        if let source = environment.makeSource() {
            do {
                try source.start { [weak fresh] observation in
                    guard let fresh else { return }
                    _ = try? fresh.record(observation, owner: owner)
                }
            } catch RecorderError.permissionRequired(let permission, let state) {
                return ["ok": false, "error": "permission_required", "permission": permission, "status": state]
            } catch {
                return ["ok": false, "error": "observer_failed", "detail": "\(error)"]
            }
            self.source = source
        }
        session = fresh
        if limits.screenshotsEnabled {
            let queue = DispatchQueue(label: "quansio.recorder.screenshots", qos: .utility)
            let timer = DispatchSource.makeTimerSource(queue: queue)
            let interval = max(limits.screenshotMinIntervalMs, 50)
            timer.schedule(deadline: .now() + .milliseconds(interval), repeating: .milliseconds(interval))
            timer.setEventHandler { [weak fresh] in
                guard let fresh else { return }
                _ = try? fresh.tickScreenshot(owner: owner)
            }
            timer.resume()
            self.timer = timer
        }
        return ["ok": true, "recording_id": recordingId, "state": "recording"]
    }

    // MARK: guarded session calls

    private func guarded(
        _ request: [String: Any], _ body: (RecorderSession, RecordingOwner) throws -> [String: Any]
    ) -> [String: Any] {
        guard let owner = RecordingOwner(wire: request["owner"]) else {
            return ["ok": false, "error": "invalid_request", "detail": "the request needs its owner {tenant_id, user_id, epoch}"]
        }
        lock.lock()
        let current = session
        lock.unlock()
        guard let current else {
            return ["ok": false, "error": "no_recording", "detail": "this recorder holds no recording"]
        }
        do {
            var out = try body(current, owner)
            out["ok"] = true
            return out
        } catch RecorderError.ownerChanged {
            // The account changed: stop observing now. The session already discarded what it held.
            lock.lock()
            endLocked()
            lock.unlock()
            return ["ok": false, "error": "owner_changed", "detail": "the call's owner or epoch is not the recording's"]
        } catch RecorderError.expired {
            lock.lock()
            endLocked()
            lock.unlock()
            return ["ok": false, "error": "expired", "detail": "the recording ran past its lifetime and was discarded"]
        } catch RecorderError.notRecording(let state) {
            return ["ok": false, "error": "not_recording", "detail": state]
        } catch RecorderError.archiveTooLarge(let bytes, let limit) {
            return ["ok": false, "error": "archive_too_large", "detail": "\(bytes) bytes; the limit is \(limit)"]
        } catch RecorderError.invalidRequest(let detail) {
            return ["ok": false, "error": "invalid_request", "detail": detail]
        } catch {
            return ["ok": false, "error": "internal", "detail": "\(error)"]
        }
    }

    /// The session, for tests.
    public var currentSession: RecorderSession? {
        lock.lock()
        defer { lock.unlock() }
        return session
    }
}

/// The wire form of a permission status (DOMAIN.md 8.6.1 E).
public func permissionJSON(_ status: PermissionStatus) -> [String: String] {
    [
        "accessibility": status.accessibility.rawValue, "screen_recording": status.screenRecording.rawValue,
        "input_monitoring": status.inputMonitoring.rawValue, "microphone": status.microphone.rawValue,
    ]
}
