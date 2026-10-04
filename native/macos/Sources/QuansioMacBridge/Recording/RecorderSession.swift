// One recording session (ADP-003, DOMAIN.md 11.7 A-C): the person's demonstration from an explicit start to an
// explicit finish, in this process's memory and nowhere else.
//
// What this type guarantees, each pinned by a test:
//  * it finishes only when `finish` is called -- no limit, timer or quiet period ends a recording as finished;
//    running out of time aborts it and discards what it held;
//  * every call names the owner and epoch it was made under; any other owner makes the session throw
//    `ownerChanged` and discard everything it buffered, once and for good;
//  * nothing is kept that redaction removes: typed text, attributes, titles and narration are redacted before
//    they are stored, text typed into a secure field is dropped entirely, and capture pauses while secure
//    event input is on;
//  * everything is bounded by the limits the server handed over.

import CryptoKit
import Foundation

private func rfc3339(_ date: Date) -> String {
    let formatter = ISO8601DateFormatter()
    formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
    formatter.timeZone = TimeZone(identifier: "UTC")
    return formatter.string(from: date)
}

/// What a `record` call did with an observation.
public enum RecordOutcome: Equatable, Sendable {
    case recorded(seq: Int)
    /// Capture is paused (secure input, a secure field); the observation was discarded unread.
    case paused(reason: String)
    /// The action limit is spent; the observation was dropped and the recording carries on.
    case droppedForLimit
    /// A keystroke in a secure field: dropped without a trace beyond the pause.
    case droppedSensitive
}

/// What the session reports about itself. Counts only: never a value.
public struct RecorderStatus: Equatable, Sendable {
    public let state: String
    public let actions: Int
    public let screenshots: Int
    public let screenshotsWithheld: Int
    public let transcriptSegments: Int
    public let pausedReason: String?
    public let droppedForLimit: Int
    public let counts: [String: Int]
}

/// The finished archive: its bytes, their digest and what they hold.
public struct FinishedArchive: Equatable, Sendable {
    public let data: Data
    public let sha256: String
    public let requestId: String
    public let actions: Int
    public let screenshots: Int
}

private struct ArchivedAction {
    var seq: Int
    var at: Date
    var kind: RecordedActionKind
    var bundleId: String
    var appName: String
    var target: [String: String]
    var windowTitle: String?
    var text: String?
    var key: String?
    var secure: Bool
    var unresolved: Bool
    var point: (x: Double, y: Double)?

    var wire: [String: Any] {
        var out: [String: Any] = [
            "seq": seq, "at": rfc3339(at), "kind": kind.rawValue,
            "app": ["bundle_id": bundleId, "name": appName],
        ]
        if !target.isEmpty { out["target"] = target }
        if let windowTitle { out["window_title"] = windowTitle }
        if let text { out["text"] = text }
        if let key { out["key"] = key }
        if secure { out["secure"] = true }
        if unresolved {
            out["target_unresolved"] = true
            if let point { out["point"] = ["x": point.x, "y": point.y] }
        }
        return out
    }
}

public final class RecorderSession: @unchecked Sendable {
    public enum State: Equatable, Sendable {
        case recording
        case finished
        case cancelled
        case aborted(String)

        public var name: String {
            switch self {
            case .recording: return "recording"
            case .finished: return "finished"
            case .cancelled: return "cancelled"
            case .aborted(let reason): return "aborted:\(reason)"
            }
        }
    }

    public let recordingId: String
    public let owner: RecordingOwner
    public let limits: RecordingLimits
    private let clock: RecorderClock
    private let secureInput: SecureInputProbe
    private let sampler: ScreenshotSampler?
    private let lock = NSLock()
    private let startedAt: Date

    private var state: State = .recording
    private var actions: [ArchivedAction] = []
    private var apps: [(bundleId: String, name: String)] = []
    private var transcript: [(at: Date, text: String)] = []
    private var transcriptChars = 0
    private var counts = RedactionCounts()
    private var pauses: [(from: Date, to: Date, reason: String)] = []
    private var openPause: (from: Date, reason: String)?
    private var nextSeq = 1
    private var dropped = 0
    private var finishedArchive: FinishedArchive?

    public init(
        recordingId: String, owner: RecordingOwner, limits: RecordingLimits, clock: RecorderClock = SystemRecorderClock(),
        secureInput: SecureInputProbe = LiveSecureInputProbe(), sampler: ScreenshotSampler? = nil
    ) {
        self.recordingId = recordingId
        self.owner = owner
        self.limits = limits
        self.clock = clock
        self.secureInput = secureInput
        self.sampler = sampler
        self.startedAt = clock.now()
    }

    // MARK: fence

    /// Apply the owner fence, the lifetime and the state. Callers hold the lock.
    private func guardActive(_ caller: RecordingOwner) throws {
        if caller != owner {
            if state == .recording || state == .finished {
                abortLocked("owner_changed")
            }
            throw RecorderError.ownerChanged
        }
        guard state == .recording else { throw RecorderError.notRecording(state: state.name) }
        if clock.now().timeIntervalSince(startedAt) * 1000 > Double(limits.maxDurationMs) {
            abortLocked("expired")
            throw RecorderError.expired
        }
    }

    /// End without finishing and discard everything held. Callers hold the lock.
    private func abortLocked(_ reason: String) {
        state = .aborted(reason)
        wipeLocked()
    }

    private func wipeLocked() {
        actions.removeAll()
        apps.removeAll()
        transcript.removeAll()
        transcriptChars = 0
        counts = RedactionCounts()
        pauses.removeAll()
        openPause = nil
        finishedArchive = nil
        sampler?.wipe()
    }

    /// The session's state, for the control channel and tests.
    public var currentState: State {
        lock.lock()
        defer { lock.unlock() }
        return state
    }

    // MARK: pausing

    private func setPause(_ reason: String?, at: Date) {
        if let open = openPause {
            if open.reason == reason { return }
            pauses.append((open.from, max(at, open.from), open.reason))
            openPause = nil
        }
        if let reason { openPause = (at, reason) }
    }

    // MARK: observations

    private func clean(_ text: String?) -> String? {
        guard let text else { return nil }
        let result = redactText(text)
        counts.merge(result.counts)
        return String(result.text.prefix(limits.attributeMaxChars))
    }

    /// Record one thing the person did, redacted, or discard it if capture is paused.
    @discardableResult
    public func record(_ observation: Observation, owner caller: RecordingOwner) throws -> RecordOutcome {
        lock.lock()
        defer { lock.unlock() }
        try guardActive(caller)
        let at = min(max(observation.at, startedAt), max(clock.now(), startedAt))
        if secureInput.isSecureInputActive() {
            setPause("secure_input", at: at)
            return .paused(reason: "secure_input")
        }
        let rule = observation.target.flatMap(fieldRule)
        var secure = false
        if case .secure? = rule {
            secure = true
            setPause("secure_field", at: at)
            switch observation.kind {
            case .key, .scroll:
                // Keystrokes into a secure field are never kept, not even as a count of keys.
                return .droppedSensitive
            default:
                break
            }
        } else {
            setPause(nil, at: at)
        }
        if actions.count >= limits.maxActions {
            dropped += 1
            return .droppedForLimit
        }
        // Redact first, truncate after: a cut can never leave half a secret that no rule would match.
        var text: String?
        if let raw = observation.text, !raw.isEmpty {
            let result = redactFieldText(observation.target ?? FieldAttributes(), raw)
            counts.merge(result.counts)
            text = result.text.map { String($0.prefix(limits.actionTextMaxChars)) }
            if result.text == nil { secure = true }
        }
        var target: [String: String] = [:]
        if let attrs = observation.target {
            for (key, value) in [
                ("ax_role", attrs.role), ("ax_subrole", attrs.subrole), ("ax_title", attrs.title),
                ("ax_identifier", attrs.identifier), ("ax_description", attrs.descriptionText),
            ] {
                if let value, !value.isEmpty, let cleaned = clean(value) { target[key] = cleaned }
            }
        }
        let appName = clean(observation.appName) ?? observation.bundleId
        if !apps.contains(where: { $0.bundleId == observation.bundleId }) {
            apps.append((observation.bundleId, appName))
        }
        let unresolved = observation.targetUnresolved
        let action = ArchivedAction(
            seq: nextSeq, at: at, kind: observation.kind, bundleId: observation.bundleId, appName: appName,
            target: target, windowTitle: clean(observation.windowTitle), text: secure ? nil : text,
            key: observation.key.flatMap { clean($0).map { String($0.prefix(64)) } }, secure: secure,
            unresolved: unresolved, point: unresolved ? observation.point : nil)
        nextSeq += 1
        actions.append(action)
        return .recorded(seq: action.seq)
    }

    /// Append a segment of the person's narration, redacted. Only a recording whose person turned narration
    /// on is ever given any.
    public func appendTranscript(_ text: String, at: Date? = nil, owner caller: RecordingOwner) throws {
        lock.lock()
        defer { lock.unlock() }
        try guardActive(caller)
        if secureInput.isSecureInputActive() { return }
        guard transcript.count < limits.transcriptMaxSegments else { return }
        let result = redactText(text)
        counts.merge(result.counts)
        let room = limits.transcriptMaxChars - transcriptChars
        guard room > 0 else { return }
        let kept = String(result.text.prefix(room))
        transcriptChars += kept.count
        let when = min(max(at ?? clock.now(), startedAt), max(clock.now(), startedAt))
        transcript.append((when, kept))
    }

    /// One periodic screenshot attempt. Nothing is captured while capture is paused.
    @discardableResult
    public func tickScreenshot(owner caller: RecordingOwner) throws -> SampleOutcome {
        lock.lock()
        defer { lock.unlock() }
        try guardActive(caller)
        guard let sampler else { return .unavailable }
        if secureInput.isSecureInputActive() {
            setPause("secure_input", at: clock.now())
            return .unavailable
        }
        if let open = openPause, open.reason == "secure_field" { return .unavailable }
        setPause(nil, at: clock.now())
        return sampler.sample(at: clock.now())
    }

    /// The session's own account of itself.
    public func status(owner caller: RecordingOwner) throws -> RecorderStatus {
        lock.lock()
        defer { lock.unlock() }
        try guardActive(caller)
        return RecorderStatus(
            state: state.name, actions: actions.count, screenshots: sampler?.kept.count ?? 0,
            screenshotsWithheld: sampler?.withheld ?? 0, transcriptSegments: transcript.count,
            pausedReason: openPause?.reason, droppedForLimit: dropped, counts: summaryLocked())
    }

    private func summaryLocked() -> [String: Int] {
        var total = counts
        if let sampler { total.merge(sampler.withheldCounts) }
        return total.summary
    }

    // MARK: ending

    /// Finish the recording: the one transition to `finished`, and only the person's explicit command reaches
    /// it. Returns the archive. A repeated finish with the same request id returns the same archive.
    public func finish(owner caller: RecordingOwner, requestId: String) throws -> FinishedArchive {
        lock.lock()
        defer { lock.unlock() }
        if let done = finishedArchive, caller == owner, state == .finished {
            if done.requestId == requestId { return done }
            throw RecorderError.notRecording(state: state.name)
        }
        try guardActive(caller)
        let finishedAt = max(clock.now(), actions.last?.at ?? startedAt)
        setPause(nil, at: finishedAt)
        var data = try buildArchive(finishedAt: finishedAt, requestId: requestId)
        while data.count > limits.archiveMaxBytes {
            guard let sampler, sampler.dropOldest() else {
                throw RecorderError.archiveTooLarge(bytes: data.count, limit: limits.archiveMaxBytes)
            }
            data = try buildArchive(finishedAt: finishedAt, requestId: requestId)
        }
        let sha = SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
        let archive = FinishedArchive(
            data: data, sha256: sha, requestId: requestId, actions: actions.count,
            screenshots: sampler?.kept.count ?? 0)
        finishedArchive = archive
        state = .finished
        return archive
    }

    /// Cancel: nothing is finished, everything held is discarded.
    public func cancel(owner caller: RecordingOwner) throws {
        lock.lock()
        defer { lock.unlock() }
        try guardActive(caller)
        state = .cancelled
        wipeLocked()
    }

    private func buildArchive(finishedAt: Date, requestId: String) throws -> Data {
        let shots: [[String: Any]] = (sampler?.kept ?? []).enumerated().map { index, shot in
            [
                "seq": index + 1, "at": rfc3339(max(min(shot.at, finishedAt), startedAt)), "sha256": shot.sha256,
                "bytes": shot.png.count, "media_type": "image/png", "width": shot.width, "height": shot.height,
                "data_base64": shot.png.base64EncodedString(), "screen_text": shot.screenText,
            ]
        }
        var total = counts
        if let sampler { total.merge(sampler.withheldCounts) }
        let document: [String: Any] = [
            "schema_version": "v1",
            "recording_id": recordingId,
            "owner": owner.wire,
            "platform": "macos",
            "started_at": rfc3339(startedAt),
            "finished_at": rfc3339(finishedAt),
            "finish": ["by": "user", "request_id": requestId],
            "apps": apps.map { ["bundle_id": $0.bundleId, "name": $0.name] },
            "actions": actions.map(\.wire),
            "screenshots": shots,
            "transcript": transcript.map { ["at": rfc3339($0.at), "text": $0.text] },
            "paused": pauses.map { ["from": rfc3339($0.from), "to": rfc3339($0.to), "reason": $0.reason] },
            "redaction": [
                "rule_set": recordingRuleSet, "counts": total.summary,
                "screenshots_withheld": sampler?.withheld ?? 0,
            ] as [String: Any],
            "limits": limits.wire,
        ]
        return try JSONSerialization.data(withJSONObject: document, options: [.sortedKeys, .withoutEscapingSlashes])
    }
}
