// The recorder's vocabulary (ADP-003, DOMAIN.md 11.7): who owns a recording, what it may capture, what an
// observation of the person is, and how the narrow platform surface is shaped so another platform can follow.
//
// Canonical owner: native/macos. This file holds shapes and bounds only; it decides no capability, policy or
// approval. The server hands the person's recorder its limits when a recording starts and checks every one of
// them again when the archive arrives, so a recorder that ignores a limit gains nothing.

import Foundation

/// The fence every recorder call carries: the signed-in account and the epoch the desktop advances when the
/// account changes (DOMAIN.md 11.7 B). A call that carries any other value is `ownerChanged`.
public struct RecordingOwner: Equatable, Hashable, Sendable {
    public let tenantId: String
    public let userId: String
    public let epoch: Int

    public init(tenantId: String, userId: String, epoch: Int) {
        self.tenantId = tenantId
        self.userId = userId
        self.epoch = epoch
    }

    /// The wire form, with the keys the server uses.
    public var wire: [String: Any] {
        ["tenant_id": tenantId, "user_id": userId, "epoch": epoch]
    }

    public init?(wire: Any?) {
        guard let map = wire as? [String: Any],
            let tenant = map["tenant_id"] as? String,
            let user = map["user_id"] as? String,
            let epoch = map["epoch"] as? Int
        else { return nil }
        self.init(tenantId: tenant, userId: user, epoch: epoch)
    }
}

/// What the recorder may capture, as `StartRecording` returns it from `config/recording.yaml`.
public struct RecordingLimits: Equatable, Sendable {
    public var maxActions: Int
    public var actionTextMaxChars: Int
    public var attributeMaxChars: Int
    public var screenshotsEnabled: Bool
    public var screenshotMaxCount: Int
    public var screenshotMinIntervalMs: Int
    public var screenshotMaxBytes: Int
    public var screenshotMaxDimensionPx: Int
    public var screenTextMaxChars: Int
    public var transcriptMaxSegments: Int
    public var transcriptMaxChars: Int
    public var archiveMaxBytes: Int
    public var maxDurationMs: Int

    public init(
        maxActions: Int = 2000, actionTextMaxChars: Int = 2000, attributeMaxChars: Int = 200,
        screenshotsEnabled: Bool = true, screenshotMaxCount: Int = 24, screenshotMinIntervalMs: Int = 5000,
        screenshotMaxBytes: Int = 262_144, screenshotMaxDimensionPx: Int = 1280, screenTextMaxChars: Int = 2000,
        transcriptMaxSegments: Int = 400, transcriptMaxChars: Int = 20_000, archiveMaxBytes: Int = 8_388_608,
        maxDurationMs: Int = 1_800_000
    ) {
        self.maxActions = maxActions
        self.actionTextMaxChars = actionTextMaxChars
        self.attributeMaxChars = attributeMaxChars
        self.screenshotsEnabled = screenshotsEnabled
        self.screenshotMaxCount = screenshotMaxCount
        self.screenshotMinIntervalMs = screenshotMinIntervalMs
        self.screenshotMaxBytes = screenshotMaxBytes
        self.screenshotMaxDimensionPx = screenshotMaxDimensionPx
        self.screenTextMaxChars = screenTextMaxChars
        self.transcriptMaxSegments = transcriptMaxSegments
        self.transcriptMaxChars = transcriptMaxChars
        self.archiveMaxBytes = archiveMaxBytes
        self.maxDurationMs = maxDurationMs
    }

    /// Read the limits object `StartRecording` returns (`config/recording.yaml: limits`), plus the
    /// recording's lifetime. A missing or malformed field is `nil`: the recorder does not guess a bound.
    public init?(wire: Any?, maxDurationMs: Int) {
        guard let map = wire as? [String: Any],
            let shots = map["screenshots"] as? [String: Any],
            let transcript = map["transcript"] as? [String: Any],
            let archive = map["archive"] as? [String: Any],
            let maxActions = map["max_actions"] as? Int,
            let textMax = map["action_text_max_chars"] as? Int,
            let attributeMax = map["attribute_max_chars"] as? Int,
            let enabled = shots["enabled"] as? Bool,
            let maxCount = shots["max_count"] as? Int,
            let interval = shots["min_interval_ms"] as? Int,
            let shotBytes = shots["max_bytes"] as? Int,
            let dimension = shots["max_dimension_px"] as? Int,
            let screenText = shots["screen_text_max_chars"] as? Int,
            let segments = transcript["max_segments"] as? Int,
            let transcriptChars = transcript["max_chars"] as? Int,
            let archiveBytes = archive["max_bytes"] as? Int
        else { return nil }
        self.init(
            maxActions: maxActions, actionTextMaxChars: textMax, attributeMaxChars: attributeMax,
            screenshotsEnabled: enabled, screenshotMaxCount: maxCount, screenshotMinIntervalMs: interval,
            screenshotMaxBytes: shotBytes, screenshotMaxDimensionPx: dimension, screenTextMaxChars: screenText,
            transcriptMaxSegments: segments, transcriptMaxChars: transcriptChars, archiveMaxBytes: archiveBytes,
            maxDurationMs: maxDurationMs)
    }

    /// The echo the archive carries under `limits`.
    public var wire: [String: Any] {
        [
            "max_actions": maxActions, "action_text_max_chars": actionTextMaxChars,
            "attribute_max_chars": attributeMaxChars,
            "screenshots": [
                "enabled": screenshotsEnabled, "max_count": screenshotMaxCount,
                "min_interval_ms": screenshotMinIntervalMs, "max_bytes": screenshotMaxBytes,
                "max_dimension_px": screenshotMaxDimensionPx, "screen_text_max_chars": screenTextMaxChars,
            ],
            "transcript": ["max_segments": transcriptMaxSegments, "max_chars": transcriptMaxChars],
            "archive": ["max_bytes": archiveMaxBytes],
        ]
    }
}

/// The action kinds a recorder reports (DOMAIN.md 11.7 C).
public enum RecordedActionKind: String, CaseIterable, Sendable {
    case click
    case doubleClick = "double_click"
    case rightClick = "right_click"
    case type
    case key
    case scroll
    case focus
    case appSwitch = "app_switch"
    case menuSelect = "menu_select"
    case setValue = "set_value"
}

/// One thing the person did, before redaction. The session redacts it before it is kept.
public struct Observation: Equatable, Sendable {
    public var kind: RecordedActionKind
    public var bundleId: String
    public var appName: String
    public var target: FieldAttributes?
    public var windowTitle: String?
    public var text: String?
    public var key: String?
    public var targetUnresolved: Bool
    public var point: (x: Double, y: Double)?
    public var at: Date

    public init(
        kind: RecordedActionKind, bundleId: String, appName: String, target: FieldAttributes? = nil,
        windowTitle: String? = nil, text: String? = nil, key: String? = nil, targetUnresolved: Bool = false,
        point: (x: Double, y: Double)? = nil, at: Date
    ) {
        self.kind = kind
        self.bundleId = bundleId
        self.appName = appName
        self.target = target
        self.windowTitle = windowTitle
        self.text = text
        self.key = key
        self.targetUnresolved = targetUnresolved
        self.point = point
        self.at = at
    }

    public static func == (lhs: Observation, rhs: Observation) -> Bool {
        lhs.kind == rhs.kind && lhs.bundleId == rhs.bundleId && lhs.appName == rhs.appName
            && lhs.target == rhs.target && lhs.windowTitle == rhs.windowTitle && lhs.text == rhs.text
            && lhs.key == rhs.key && lhs.targetUnresolved == rhs.targetUnresolved
            && lhs.point?.x == rhs.point?.x && lhs.point?.y == rhs.point?.y && lhs.at == rhs.at
    }
}

/// Where the recorder's clock comes from, so a test can drive time without waiting for it.
public protocol RecorderClock: Sendable {
    func now() -> Date
}

public struct SystemRecorderClock: RecorderClock {
    public init() {}
    public func now() -> Date { Date() }
}

/// Why a recorder call was refused.
public enum RecorderError: Error, Equatable, Sendable {
    /// The call's tenant, user or epoch is not the recording's: the account changed. The recorder has
    /// discarded everything it buffered (DOMAIN.md 11.7 B, `RECORDING_OWNER_CHANGED` on the server).
    case ownerChanged
    /// The session is not recording (finished, cancelled or aborted).
    case notRecording(state: String)
    /// The recording ran past its lifetime without the person finishing it: it is aborted, never completed.
    case expired
    /// The archive would be larger than the archive limit even without its screenshots.
    case archiveTooLarge(bytes: Int, limit: Int)
    /// A permission the recorder needs is missing; nothing is captured until it is `granted`.
    case permissionRequired(permission: String, status: String)
    /// A request id was reused with other arguments.
    case idempotencyMismatch
    /// The request was malformed.
    case invalidRequest(String)
}

/// The narrow surface a platform provides, so Windows can implement it later (DOMAIN.md 11.7 C).
public protocol ObservationSource: AnyObject, Sendable {
    /// Begin delivering observations of the person. Throws `permissionRequired` when the platform will not
    /// let the recorder watch.
    func start(deliver: @escaping @Sendable (Observation) -> Void) throws
    /// Stop delivering. Idempotent.
    func stop()
}
