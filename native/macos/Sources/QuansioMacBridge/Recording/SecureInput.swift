// Secure input detection (ADP-003, DOMAIN.md 11.7 C).
//
// While a password field (or anything that asked the system for secure event input) has the keyboard, the
// recorder captures nothing: not the keys, not a screenshot. The system answer is the authority here, and it
// is read, never assumed: `IsSecureEventInputEnabled()` is true exactly while some process holds secure
// event input.

import Carbon.HIToolbox
import Foundation

/// Tells the recorder whether the system is in secure input mode.
public protocol SecureInputProbe: Sendable {
    func isSecureInputActive() -> Bool
}

/// The running system's answer.
public struct LiveSecureInputProbe: SecureInputProbe {
    public init() {}

    public func isSecureInputActive() -> Bool {
        IsSecureEventInputEnabled()
    }
}

/// Take secure event input for this process, as a password field does. For the test that proves the live
/// probe sees the real system state; production recorder code never calls this.
public enum SecureInputHold {
    /// Enable secure input; returns a closure that releases it. Calls nest, as the system's own do.
    public static func acquire() -> () -> Void {
        EnableSecureEventInput()
        return { DisableSecureEventInput() }
    }
}
