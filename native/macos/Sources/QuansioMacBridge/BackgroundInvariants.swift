// Background-operation invariants and receipted actions (ADP-002, DOMAIN.md 8.6.1 A and C).
//
// An action that claims to run in the background promises three things: nothing arrives from the system
// input stream while it runs, the real pointer does not move, and the frontmost application is the same
// one afterwards. This file measures them around the action and attaches the measurement to the receipt;
// it does not trust the action's own claim, and it does not decide what a violation means (the Rust
// runtime does, and re-derives it from the numbers).
//
// "Physical input" is any event on the system input stream during the window. The bridge cannot tell a
// person from a synthetic event posted into that stream, and either one breaks a claim of not touching
// it, so both count.

import AppKit
import CoreGraphics
import Foundation

/// What the measurement needs from the system, so the logic can be driven without moving the machine.
public protocol SystemProbe: Sendable {
    /// The frontmost application, or nil when there is none or it cannot be identified.
    func frontmost() -> FrontmostApp?
    /// The real pointer position, or nil when there is no window-server session to ask.
    func cursorLocation() -> CGPoint?
    /// Seconds since the last event of any kind from the system input stream.
    func secondsSinceLastInput() -> TimeInterval
    /// Monotonic seconds.
    func now() -> TimeInterval
}

/// The running system.
public struct LiveSystemProbe: SystemProbe {
    private let input = LiveInputClock()

    public init() {}

    public func frontmost() -> FrontmostApp? {
        guard let app = try? frontmostApp() else { return nil }
        return FrontmostApp(bundleId: app.bundleId, pid: app.pid)
    }

    public func cursorLocation() -> CGPoint? {
        CGEvent(source: nil)?.location
    }

    public func secondsSinceLastInput() -> TimeInterval { input.secondsSinceLastInput() }

    public func now() -> TimeInterval { input.now() }
}

/// Whether an input event arrived during a window, from two idle readings.
///
/// With no input the idle time grows by exactly the elapsed time; an event inside the window leaves it
/// shorter. `epsilon` absorbs the difference between the two clocks' granularity.
public func physicalInputDetected(
    idleBefore: TimeInterval,
    idleAfter: TimeInterval,
    elapsed: TimeInterval,
    epsilon: TimeInterval = 0.05
) -> Bool {
    idleAfter < idleBefore + elapsed - epsilon
}

/// Run `body` and measure the background invariants around it.
///
/// Returns nil for the measurement when it cannot be made (no frontmost application readable before or
/// after, or no pointer to read): an unmeasured claim is reported as unmeasured, never as clean.
public func measureBackground<T>(
    probe: SystemProbe = LiveSystemProbe(),
    _ body: () throws -> T
) rethrows -> (value: T, invariants: BackgroundInvariants?) {
    let frontBefore = probe.frontmost()
    let cursorBefore = probe.cursorLocation()
    let idleBefore = probe.secondsSinceLastInput()
    let started = probe.now()
    let value = try body()
    let elapsed = probe.now() - started
    let idleAfter = probe.secondsSinceLastInput()
    let cursorAfter = probe.cursorLocation()
    let frontAfter = probe.frontmost()
    guard let frontBefore, let frontAfter, let cursorBefore, let cursorAfter else {
        return (value, nil)
    }
    let invariants = BackgroundInvariants(
        physicalInput: physicalInputDetected(idleBefore: idleBefore, idleAfter: idleAfter, elapsed: elapsed),
        movedSystemCursor: cursorBefore != cursorAfter,
        frontmostBefore: frontBefore,
        frontmostAfter: frontAfter
    )
    return (value, invariants)
}

// MARK: verification

/// Whether typed text can be seen in a field's value after typing.
///
/// `nil` when the value could not be read (a secure field, or no Accessibility): unknown, not false.
/// `true` when the value now contains the text and did not before (or grew by it); `false` when the value
/// was readable and the text is not in it.
public func typedTextVerification(typed: String, valueBefore: String?, valueAfter: String?) -> Bool? {
    guard let valueAfter else { return nil }
    if typed.isEmpty { return nil }
    guard valueAfter.contains(typed) else { return false }
    guard let valueBefore else { return true }
    // Text that was already there proves nothing: require the count of occurrences to have grown.
    return valueAfter.components(separatedBy: typed).count > valueBefore.components(separatedBy: typed).count
}

/// The receipt for a pasteboard write, measured and read back.
///
/// The write itself is `setClipboardText`, which refuses unless the exact authorized application holds
/// the foreground. The verification is the pasteboard's own value afterwards, which is exactly what the
/// write was meant to produce.
public func receiptedClipboardWrite(
    _ text: String,
    expectedBundleId: String,
    probe: SystemProbe = LiveSystemProbe()
) throws -> ActionReceipt {
    try requireForegroundApp(expectedBundleId)
    let (_, invariants) = try measureBackground(probe: probe) {
        try setClipboardText(text, expectedBundleId: expectedBundleId)
    }
    let base: ActionReceipt = clipboardText() == text
        ? .applied(.pasteboard, .background)
        : .notApplied(.pasteboard, .background)
    return base.measured(invariants)
}

/// The receipt for a pasteboard read: data came back, there was no effect to verify.
public func receiptedClipboardRead(
    expectedBundleId: String,
    probe: SystemProbe = LiveSystemProbe()
) throws -> (text: String, receipt: ActionReceipt) {
    try requireForegroundApp(expectedBundleId)
    let (text, invariants) = measureBackground(probe: probe) { clipboardText() }
    return (text, ActionReceipt.observed(.pasteboard, .background).measured(invariants))
}

/// The receipt for typed text. Input goes through the system input stream, so the action is foreground;
/// the effect is verified only when the focused field's value can be read before and after.
public func receiptedTypeText(
    _ text: String,
    expectedBundleId: String,
    focusedValue: () -> String? = focusedFieldValue
) throws -> ActionReceipt {
    let before = focusedValue()
    try typeText(text, expectedBundleId: expectedBundleId)
    let after = focusedValue()
    switch typedTextVerification(typed: text, valueBefore: before, valueAfter: after) {
    case .some(true): return .applied(.hidEvent, .foreground)
    case .some(false): return .notApplied(.hidEvent, .foreground)
    case .none: return .unverified(.hidEvent, .foreground)
    }
}

/// The receipt for a click through the system input stream. A click has no read-back that is true in
/// general (what it did is whatever the application decided), so its effect is unverified.
public func receiptedClick(
    x: Double,
    y: Double,
    button: MouseButton = .left,
    expectedBundleId: String
) throws -> ActionReceipt {
    try click(x: x, y: y, button: button, expectedBundleId: expectedBundleId)
    return .unverified(.hidEvent, .foreground)
}

/// The value of the system-wide focused UI element, when Accessibility allows reading it.
public func focusedFieldValue() -> String? {
    guard automationTrusted() else { return nil }
    let system = AXUIElementCreateSystemWide()
    var focused: CFTypeRef?
    guard AXUIElementCopyAttributeValue(system, kAXFocusedUIElementAttribute as CFString, &focused) == .success,
          let element = focused else { return nil }
    var value: CFTypeRef?
    // `as!` is safe: a successful focused-element copy returns an AXUIElement.
    guard AXUIElementCopyAttributeValue(element as! AXUIElement, kAXValueAttribute as CFString, &value) == .success,
          let text = value as? String else { return nil }
    return text
}
