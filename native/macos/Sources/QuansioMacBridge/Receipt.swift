// Action receipts (ADP-002, DOMAIN.md 8.6.1 A and C).
//
// What a native computer action says about itself. The wire form is the one the Rust runtime parses
// strictly (`quansio_machine::computer_use::receipt`) and the Tool Registry schema declares
// (`config/tools.yaml`); the fixtures under Tests/.../Fixtures hold the three to one shape.
//
// The one rule that matters here: `effectVerified` is `true` only when this bridge observed the intended
// effect, `false` when it observed the effect to be absent (or a background claim was broken), and `nil`
// -- encoded as JSON `null` -- when it could not observe at all. `nil` is never rounded to either.

import Foundation

public enum ReceiptMethod: String, Codable, Sendable, Equatable {
    case accessibilityTree = "accessibility_tree"
    case accessibilityAction = "accessibility_action"
    case processEvent = "process_event"
    case hidEvent = "hid_event"
    case pasteboard
}

public enum ExecutionMethod: String, Codable, Sendable, Equatable {
    case background
    case foreground
}

public enum ReceiptOutcome: String, Codable, Sendable, Equatable {
    case applied
    case observed
    case unverified
    case notApplied = "not_applied"
    case invariantViolated = "invariant_violated"
    case refused
}

public struct FrontmostApp: Codable, Sendable, Equatable {
    public let bundleId: String
    public let pid: Int32

    public init(bundleId: String, pid: Int32) {
        self.bundleId = bundleId
        self.pid = pid
    }

    enum CodingKeys: String, CodingKey {
        case bundleId = "bundle_id"
        case pid
    }
}

/// What was measured around a background action.
public struct BackgroundInvariants: Codable, Sendable, Equatable {
    public let physicalInput: Bool
    public let movedSystemCursor: Bool
    public let frontmostBefore: FrontmostApp
    public let frontmostAfter: FrontmostApp
    public let frontmostChanged: Bool

    public init(
        physicalInput: Bool,
        movedSystemCursor: Bool,
        frontmostBefore: FrontmostApp,
        frontmostAfter: FrontmostApp
    ) {
        self.physicalInput = physicalInput
        self.movedSystemCursor = movedSystemCursor
        self.frontmostBefore = frontmostBefore
        self.frontmostAfter = frontmostAfter
        self.frontmostChanged = frontmostBefore != frontmostAfter
    }

    /// Whether the measurement shows a broken background claim.
    public var violated: Bool {
        physicalInput || movedSystemCursor || frontmostChanged || frontmostBefore != frontmostAfter
    }

    enum CodingKeys: String, CodingKey {
        case physicalInput = "physical_input"
        case movedSystemCursor = "moved_system_cursor"
        case frontmostBefore = "frontmost_before"
        case frontmostAfter = "frontmost_after"
        case frontmostChanged = "frontmost_changed"
    }
}

/// The receipt of one native computer action.
public struct ActionReceipt: Codable, Sendable, Equatable {
    public var method: ReceiptMethod
    public var executionMethod: ExecutionMethod
    public var dispatched: Bool
    /// `true`, `false`, or `nil` (unknown). Encoded as `null`, never omitted.
    public var effectVerified: Bool?
    public var outcome: ReceiptOutcome
    public var invariantViolated: Bool
    public var invariants: BackgroundInvariants?

    public init(
        method: ReceiptMethod,
        executionMethod: ExecutionMethod,
        dispatched: Bool,
        effectVerified: Bool?,
        outcome: ReceiptOutcome,
        invariantViolated: Bool = false,
        invariants: BackgroundInvariants? = nil
    ) {
        self.method = method
        self.executionMethod = executionMethod
        self.dispatched = dispatched
        self.effectVerified = effectVerified
        self.outcome = outcome
        self.invariantViolated = invariantViolated
        self.invariants = invariants
    }

    enum CodingKeys: String, CodingKey {
        case method
        case executionMethod = "execution_method"
        case dispatched
        case effectVerified = "effect_verified"
        case outcome
        case invariantViolated = "invariant_violated"
        case invariants
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(method, forKey: .method)
        try container.encode(executionMethod, forKey: .executionMethod)
        try container.encode(dispatched, forKey: .dispatched)
        // Not `encodeIfPresent`: an absent verdict and an unknown one are different statements.
        try container.encode(effectVerified, forKey: .effectVerified)
        try container.encode(outcome, forKey: .outcome)
        try container.encode(invariantViolated, forKey: .invariantViolated)
        try container.encode(invariants, forKey: .invariants)
    }

    // MARK: constructors

    /// Input handed to the system whose effect could not be observed.
    public static func unverified(_ method: ReceiptMethod, _ execution: ExecutionMethod) -> ActionReceipt {
        ActionReceipt(method: method, executionMethod: execution, dispatched: true, effectVerified: nil, outcome: .unverified)
    }

    /// Input whose intended effect was observed.
    public static func applied(_ method: ReceiptMethod, _ execution: ExecutionMethod) -> ActionReceipt {
        ActionReceipt(method: method, executionMethod: execution, dispatched: true, effectVerified: true, outcome: .applied)
    }

    /// Input whose intended effect was observed to be absent.
    public static func notApplied(_ method: ReceiptMethod, _ execution: ExecutionMethod) -> ActionReceipt {
        ActionReceipt(method: method, executionMethod: execution, dispatched: true, effectVerified: false, outcome: .notApplied)
    }

    /// A read that returned data.
    public static func observed(_ method: ReceiptMethod, _ execution: ExecutionMethod) -> ActionReceipt {
        ActionReceipt(method: method, executionMethod: execution, dispatched: true, effectVerified: nil, outcome: .observed)
    }

    /// Nothing was sent.
    public static func refused(_ method: ReceiptMethod, _ execution: ExecutionMethod) -> ActionReceipt {
        ActionReceipt(method: method, executionMethod: execution, dispatched: false, effectVerified: false, outcome: .refused)
    }

    // MARK: the rules

    /// Apply what this bridge can decide for itself, so what it reports never contradicts what it
    /// measured. The Rust runtime applies the same rules again and trusts neither side's flag.
    ///
    /// * A background action whose measurement shows a broken claim is `invariant_violated`, with
    ///   `effectVerified = false`, whatever was observed about the effect.
    /// * A background action reported applied without a measurement is lowered to unverified.
    public func normalized() -> ActionReceipt {
        var receipt = self
        guard receipt.executionMethod == .background, receipt.dispatched else { return receipt }
        if receipt.invariantViolated || receipt.invariants?.violated == true {
            receipt.invariantViolated = true
            receipt.effectVerified = false
            receipt.outcome = .invariantViolated
        } else if receipt.invariants == nil && receipt.effectVerified == true {
            receipt.effectVerified = nil
            receipt.outcome = .unverified
        }
        return receipt
    }

    /// The same receipt with a background measurement attached, then normalized.
    public func measured(_ invariants: BackgroundInvariants?) -> ActionReceipt {
        var receipt = self
        receipt.invariants = invariants
        return receipt.normalized()
    }
}

public enum ReceiptJSON {
    /// The wire form of a receipt, with sorted keys so fixtures compare stably.
    public static func encode(_ receipt: ActionReceipt) throws -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        return try encoder.encode(receipt)
    }
}
