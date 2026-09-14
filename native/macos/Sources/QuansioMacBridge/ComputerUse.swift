// Accessibility (AX) and input surface for computer-use (EXEC-010).
//
// Canonical owner: native/macos, called by the Rust machine module (DOSSIER.md §5 "Native computer-use:
// native broker called by Rust machine module"). This file is the *privileged* half and stays as narrow as
// it can: it reports what the system says about applications, reads a bounded slice of an accessibility
// tree, and posts input events. It decides no capability, no policy and no approval -- whether a tier may
// act on an app is the Rust machine module's decision, and this bridge only answers questions.
//
// Two bounds are deliberate. A tree read is bounded in nodes and depth, because an application's
// accessibility tree is arbitrarily large and a bridge that copies all of it is a bridge that can be made
// to allocate without limit. A screenshot is bounded in bytes, for the same reason.

import ApplicationServices
import AppKit
import CoreGraphics

/// One node of an accessibility tree, flattened for the caller.
public struct AXNode: Equatable, Sendable {
    public let role: String
    public let title: String
    public let value: String
    public let depth: Int
    public let childCount: Int

    public init(role: String, title: String, value: String, depth: Int, childCount: Int) {
        self.role = role
        self.title = title
        self.value = value
        self.depth = depth
        self.childCount = childCount
    }
}

/// What the system says an application is.
public struct AppIdentity: Equatable, Sendable {
    public let bundleId: String
    public let name: String
    public let pid: pid_t

    public init(bundleId: String, name: String, pid: pid_t) {
        self.bundleId = bundleId
        self.name = name
        self.pid = pid
    }
}

/// A bounded read of an application's accessibility tree.
public struct AXTree: Equatable, Sendable {
    public let app: AppIdentity
    public let nodes: [AXNode]
    /// Whether the walk stopped because a bound was reached rather than because the tree ended.
    public let truncated: Bool
}

/// The bridge's own failures. Every one is a refusal rather than a partial answer.
public enum ComputerUseError: Error, Equatable {
    /// This process is not trusted for Accessibility, so no AX call can succeed.
    case automationNotTrusted
    /// There is no frontmost application to ask about.
    case noForegroundApp
    /// An accessibility call failed; the code is the AX error.
    case accessibilityFailed(Int32)
    /// A requested bound was not usable.
    case invalidBound(String)
    /// Screen capture produced nothing (usually Screen Recording is not granted).
    case screenCaptureUnavailable
    /// The posted event could not be created.
    case eventUnavailable(String)
}

/// Whether this process may use the accessibility API at all.
///
/// This is a TCC permission an operator grants in System Settings; a process cannot grant itself one, so a
/// `false` here is a boundary rather than a bug and every privileged call must refuse.
public func automationTrusted() -> Bool {
    AXIsProcessTrusted()
}

/// The application the user is currently looking at.
///
/// Identity comes from `NSWorkspace` rather than from the system-wide AX element: that element's
/// focused-application attribute answers `kAXErrorCannotComplete` on a host with no attached GUI session,
/// while the workspace's frontmost application is answerable in the same situation.
public func frontmostApp() throws -> AppIdentity {
    guard let app = NSWorkspace.shared.frontmostApplication else {
        throw ComputerUseError.noForegroundApp
    }
    return AppIdentity(
        bundleId: app.bundleIdentifier ?? "",
        name: app.localizedName ?? "",
        pid: app.processIdentifier
    )
}

/// Flip the frontmost application's accessibility tree, bounded in nodes and depth.
///
/// The walk is breadth-first so the bound drops the deepest levels first, which are the least likely to
/// carry the control a caller is looking for.
public func boundedTree(pid: pid_t, maxNodes: Int = 512, maxDepth: Int = 8) throws -> AXTree {
    guard maxNodes > 0, maxDepth >= 0 else {
        throw ComputerUseError.invalidBound("maxNodes must be > 0 and maxDepth >= 0")
    }
    guard automationTrusted() else {
        throw ComputerUseError.automationNotTrusted
    }
    let app = AXUIElementCreateApplication(pid)
    var role: CFTypeRef?
    let roleError = AXUIElementCopyAttributeValue(app, kAXRoleAttribute as CFString, &role)
    guard roleError == .success else {
        throw ComputerUseError.accessibilityFailed(roleError.rawValue)
    }
    var nodes: [AXNode] = []
    var truncated = false
    var queue: [(AXUIElement, Int)] = [(app, 0)]
    while let (element, depth) = queue.first {
        queue.removeFirst()
        if nodes.count >= maxNodes {
            truncated = true
            break
        }
        nodes.append(node(for: element, depth: depth))
        if depth >= maxDepth {
            if childCount(of: element) > 0 { truncated = true }
            continue
        }
        for child in children(of: element) {
            queue.append((child, depth + 1))
        }
    }
    let resolved: AppIdentity
    do {
        resolved = try frontmostApp()
    } catch {
        resolved = AppIdentity(bundleId: "", name: "", pid: pid)
    }
    return AXTree(app: resolved, nodes: nodes, truncated: truncated)
}

private func stringAttribute(_ element: AXUIElement, _ attribute: String) -> String {
    var value: CFTypeRef?
    guard AXUIElementCopyAttributeValue(element, attribute as CFString, &value) == .success else {
        return ""
    }
    if let text = value as? String { return text }
    if let number = value as? NSNumber { return number.stringValue }
    return ""
}

private func node(for element: AXUIElement, depth: Int) -> AXNode {
    AXNode(
        role: stringAttribute(element, kAXRoleAttribute as String),
        title: stringAttribute(element, kAXTitleAttribute as String),
        value: stringAttribute(element, kAXValueAttribute as String),
        depth: depth,
        childCount: childCount(of: element)
    )
}

private func childCount(of element: AXUIElement) -> Int {
    var children: CFTypeRef?
    guard AXUIElementCopyAttributeValue(element, kAXChildrenAttribute as CFString, &children) == .success,
          let list = children as? [Any] else {
        return 0
    }
    return list.count
}

private func children(of element: AXUIElement) -> [AXUIElement] {
    var children: CFTypeRef?
    guard AXUIElementCopyAttributeValue(element, kAXChildrenAttribute as CFString, &children) == .success,
          let list = children as? [AXUIElement] else {
        return []
    }
    return list
}

/// Post a click at a screen coordinate.
public func click(x: Double, y: Double) throws {
    try post(mouseType: .leftMouseDown, x: x, y: y)
    try post(mouseType: .leftMouseUp, x: x, y: y)
}

private func post(mouseType: CGEventType, x: Double, y: Double) throws {
    guard let event = CGEvent(
        mouseEventSource: nil,
        mouseType: mouseType,
        mouseCursorPosition: CGPoint(x: x, y: y),
        mouseButton: .left
    ) else {
        throw ComputerUseError.eventUnavailable("could not build a mouse event")
    }
    event.post(tap: .cghidEventTap)
}

/// Type text into whatever holds focus.
public func typeText(_ text: String) throws {
    for scalar in text.unicodeScalars {
        var unit = UniChar(scalar.value)
        guard let down = CGEvent(keyboardEventSource: nil, virtualKey: 0, keyDown: true),
              let up = CGEvent(keyboardEventSource: nil, virtualKey: 0, keyDown: false) else {
            throw ComputerUseError.eventUnavailable("could not build a key event")
        }
        down.keyboardSetUnicodeString(stringLength: 1, unicodeString: &unit)
        up.keyboardSetUnicodeString(stringLength: 1, unicodeString: &unit)
        down.post(tap: .cghidEventTap)
        up.post(tap: .cghidEventTap)
    }
}

/// The clipboard's current text, or an empty string when it holds none.
public func clipboardText() -> String {
    NSPasteboard.general.string(forType: .string) ?? ""
}

/// Replace the clipboard's text.
public func setClipboardText(_ text: String) {
    NSPasteboard.general.clearContents()
    NSPasteboard.general.setString(text, forType: .string)
}

/// Whether the screen can actually be read, which Screen Recording decides.
public func screenCaptureAvailable() -> Bool {
    let windows = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]] ?? []
    return windows.contains { ($0[kCGWindowName as String] as? String)?.isEmpty == false }
}
