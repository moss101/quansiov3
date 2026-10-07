// The live observer (ADP-003, DOMAIN.md 11.7 C): what the person does, read from the system.
//
// It watches with a listen-only event tap (it can see input, never change or post it) and names each click's
// target through the accessibility API: the control's role, title, identifier and description and the
// application's bundle id. A control that cannot be resolved is reported as such, with the screen point as
// evidence for the person's review and never as an instruction. Typed characters are coalesced per focused
// control into one `type` observation. Needs Accessibility (the lookup) and Input Monitoring (the tap); with
// either missing it refuses to start rather than watching less than it claims.
//
// Everything it reports goes through `RecorderSession.record`, which redacts it before it is kept. This type
// decides nothing about what is kept.

import ApplicationServices
import AppKit
import Carbon.HIToolbox
import CoreGraphics
import Foundation

/// A control as the accessibility API describes it.
public struct ResolvedControl: Equatable, Sendable {
    public let pid: pid_t
    public let attributes: FieldAttributes
    public let windowTitle: String?
}

private func axString(_ element: AXUIElement, _ attribute: String) -> String? {
    var value: CFTypeRef?
    guard AXUIElementCopyAttributeValue(element, attribute as CFString, &value) == .success else { return nil }
    if let text = value as? String, !text.isEmpty { return text }
    return nil
}

private func describe(_ element: AXUIElement) -> FieldAttributes {
    FieldAttributes(
        role: axString(element, kAXRoleAttribute as String), subrole: axString(element, kAXSubroleAttribute as String),
        title: axString(element, kAXTitleAttribute as String),
        identifier: axString(element, kAXIdentifierAttribute as String),
        descriptionText: axString(element, kAXDescriptionAttribute as String))
}

private func windowTitle(of element: AXUIElement) -> String? {
    var window: CFTypeRef?
    guard AXUIElementCopyAttributeValue(element, kAXWindowAttribute as CFString, &window) == .success,
        let window, CFGetTypeID(window) == AXUIElementGetTypeID()
    else { return nil }
    return axString(window as! AXUIElement, kAXTitleAttribute as String)
}

/// The control at a screen point (origin top-left of the main display), or `nil` when it cannot be resolved.
public func controlAt(x: Double, y: Double) -> ResolvedControl? {
    var found: AXUIElement?
    guard AXUIElementCopyElementAtPosition(AXUIElementCreateSystemWide(), Float(x), Float(y), &found) == .success,
        let element = found
    else { return nil }
    var pid: pid_t = 0
    guard AXUIElementGetPid(element, &pid) == .success else { return nil }
    var attributes = describe(element)
    // A label inside a button names the button: when the element itself says nothing, ask its parent once.
    if attributes.title == nil, attributes.descriptionText == nil, attributes.identifier == nil {
        var parent: CFTypeRef?
        if AXUIElementCopyAttributeValue(element, kAXParentAttribute as CFString, &parent) == .success, let parent,
            CFGetTypeID(parent) == AXUIElementGetTypeID()
        {
            let parentAttributes = describe(parent as! AXUIElement)
            if parentAttributes.title != nil || parentAttributes.descriptionText != nil || parentAttributes.identifier != nil {
                attributes = parentAttributes
            }
        }
    }
    return ResolvedControl(pid: pid, attributes: attributes, windowTitle: windowTitle(of: element))
}

private func application(_ pid: pid_t) -> (bundleId: String, name: String)? {
    guard let app = NSRunningApplication(processIdentifier: pid), let bundle = app.bundleIdentifier, !bundle.isEmpty
    else { return nil }
    return (bundle, app.localizedName ?? bundle)
}

public final class LiveEventObserver: ObservationSource, @unchecked Sendable {
    private let lock = NSLock()
    private var deliver: (@Sendable (Observation) -> Void)?
    private var tap: CFMachPort?
    private var runLoop: CFRunLoop?
    private var workspaceObserver: NSObjectProtocol?
    private var flushTimer: DispatchSourceTimer?
    private var typing = ""
    private var typingTarget: (pid: pid_t, attributes: FieldAttributes, windowTitle: String?)?
    private var lastScroll = Date.distantPast
    private let ownPid = getpid()

    public init() {}

    public func start(deliver: @escaping @Sendable (Observation) -> Void) throws {
        guard automationTrusted() else {
            throw RecorderError.permissionRequired(permission: "accessibility", status: "denied")
        }
        self.deliver = deliver
        let mask: CGEventMask =
            (1 << CGEventType.leftMouseDown.rawValue) | (1 << CGEventType.rightMouseDown.rawValue)
            | (1 << CGEventType.keyDown.rawValue) | (1 << CGEventType.scrollWheel.rawValue)
        let callback: CGEventTapCallBack = { _, type, event, refcon in
            guard let refcon else { return Unmanaged.passUnretained(event) }
            Unmanaged<LiveEventObserver>.fromOpaque(refcon).takeUnretainedValue().handle(type: type, event: event)
            return Unmanaged.passUnretained(event)
        }
        guard
            let tap = CGEvent.tapCreate(
                tap: .cgSessionEventTap, place: .headInsertEventTap, options: .listenOnly, eventsOfInterest: mask,
                callback: callback, userInfo: Unmanaged.passUnretained(self).toOpaque())
        else {
            throw RecorderError.permissionRequired(permission: "input_monitoring", status: "denied")
        }
        self.tap = tap
        let ready = DispatchSemaphore(value: 0)
        let thread = Thread { [weak self] in
            guard let self else { return }
            let source = CFMachPortCreateRunLoopSource(kCFAllocatorDefault, tap, 0)
            self.lock.lock()
            self.runLoop = CFRunLoopGetCurrent()
            self.lock.unlock()
            CFRunLoopAddSource(CFRunLoopGetCurrent(), source, .commonModes)
            CGEvent.tapEnable(tap: tap, enable: true)
            ready.signal()
            CFRunLoopRun()
        }
        thread.name = "quansio.recorder.observer"
        thread.start()
        ready.wait()
        workspaceObserver = NSWorkspace.shared.notificationCenter.addObserver(
            forName: NSWorkspace.didActivateApplicationNotification, object: nil, queue: nil
        ) { [weak self] note in
            guard let self, let app = note.userInfo?[NSWorkspace.applicationUserInfoKey] as? NSRunningApplication,
                app.processIdentifier != self.ownPid, let bundle = app.bundleIdentifier
            else { return }
            self.flushTyping()
            self.deliver?(
                Observation(kind: .appSwitch, bundleId: bundle, appName: app.localizedName ?? bundle, at: Date()))
        }
        let timer = DispatchSource.makeTimerSource(queue: DispatchQueue(label: "quansio.recorder.typing"))
        timer.schedule(deadline: .now() + .milliseconds(1500), repeating: .milliseconds(1500))
        timer.setEventHandler { [weak self] in self?.flushTyping() }
        timer.resume()
        flushTimer = timer
    }

    public func stop() {
        flushTyping()
        flushTimer?.cancel()
        flushTimer = nil
        if let workspaceObserver { NSWorkspace.shared.notificationCenter.removeObserver(workspaceObserver) }
        workspaceObserver = nil
        if let tap { CGEvent.tapEnable(tap: tap, enable: false) }
        lock.lock()
        let loop = runLoop
        runLoop = nil
        lock.unlock()
        if let loop { CFRunLoopStop(loop) }
        tap = nil
        deliver = nil
    }

    // MARK: events

    fileprivate func handle(type: CGEventType, event: CGEvent) {
        if type == .tapDisabledByTimeout || type == .tapDisabledByUserInput {
            if let tap { CGEvent.tapEnable(tap: tap, enable: true) }
            return
        }
        guard event.getIntegerValueField(.eventSourceUnixProcessID) != Int64(ownPid) else { return }
        switch type {
        case .leftMouseDown, .rightMouseDown:
            flushTyping()
            let at = event.location
            let kind: RecordedActionKind =
                type == .rightMouseDown
                ? .rightClick : (event.getIntegerValueField(.mouseEventClickState) >= 2 ? .doubleClick : .click)
            if let control = controlAt(x: Double(at.x), y: Double(at.y)), let app = application(control.pid) {
                deliver?(
                    Observation(
                        kind: kind, bundleId: app.bundleId, appName: app.name, target: control.attributes,
                        windowTitle: control.windowTitle, at: Date()))
            } else if let front = NSWorkspace.shared.frontmostApplication, let bundle = front.bundleIdentifier {
                deliver?(
                    Observation(
                        kind: kind, bundleId: bundle, appName: front.localizedName ?? bundle, targetUnresolved: true,
                        point: (Double(at.x), Double(at.y)), at: Date()))
            }
        case .scrollWheel:
            let now = Date()
            guard now.timeIntervalSince(lastScroll) > 0.75 else { return }
            lastScroll = now
            let at = event.location
            if let control = controlAt(x: Double(at.x), y: Double(at.y)), let app = application(control.pid) {
                deliver?(
                    Observation(
                        kind: .scroll, bundleId: app.bundleId, appName: app.name, target: control.attributes,
                        windowTitle: control.windowTitle, at: now))
            }
        case .keyDown:
            handleKey(event)
        default:
            break
        }
    }

    private func focusedControl() -> (pid: pid_t, attributes: FieldAttributes, windowTitle: String?)? {
        guard let front = NSWorkspace.shared.frontmostApplication else { return nil }
        let app = AXUIElementCreateApplication(front.processIdentifier)
        var focused: CFTypeRef?
        guard AXUIElementCopyAttributeValue(app, kAXFocusedUIElementAttribute as CFString, &focused) == .success,
            let focused, CFGetTypeID(focused) == AXUIElementGetTypeID()
        else { return nil }
        let element = focused as! AXUIElement
        return (front.processIdentifier, describe(element), windowTitle(of: element))
    }

    private func handleKey(_ event: CGEvent) {
        let flags = event.flags
        let code = Int(event.getIntegerValueField(.keyboardEventKeycode))
        let named: [Int: String] = [
            kVK_Return: "return", kVK_Tab: "tab", kVK_Escape: "escape", kVK_LeftArrow: "left",
            kVK_RightArrow: "right", kVK_UpArrow: "up", kVK_DownArrow: "down", kVK_ForwardDelete: "forward_delete",
        ]
        let chord = flags.contains(.maskCommand) || flags.contains(.maskControl)
        if chord || named[code] != nil {
            flushTyping()
            guard let front = NSWorkspace.shared.frontmostApplication, let bundle = front.bundleIdentifier else { return }
            var parts: [String] = []
            if flags.contains(.maskCommand) { parts.append("cmd") }
            if flags.contains(.maskControl) { parts.append("ctrl") }
            if flags.contains(.maskAlternate) { parts.append("opt") }
            if flags.contains(.maskShift) { parts.append("shift") }
            var length = 0
            var buffer = [UniChar](repeating: 0, count: 4)
            event.keyboardGetUnicodeString(maxStringLength: 4, actualStringLength: &length, unicodeString: &buffer)
            parts.append(named[code] ?? String(utf16CodeUnits: buffer, count: length).lowercased())
            deliver?(
                Observation(
                    kind: .key, bundleId: bundle, appName: front.localizedName ?? bundle, key: parts.joined(separator: "+"),
                    at: Date()))
            return
        }
        if code == kVK_Delete {
            lock.lock()
            if !typing.isEmpty { typing.removeLast() }
            lock.unlock()
            return
        }
        var length = 0
        var buffer = [UniChar](repeating: 0, count: 8)
        event.keyboardGetUnicodeString(maxStringLength: 8, actualStringLength: &length, unicodeString: &buffer)
        guard length > 0 else { return }
        let characters = String(utf16CodeUnits: buffer, count: length)
        guard let target = focusedControl() else { return }
        lock.lock()
        if let current = typingTarget, current.pid != target.pid || current.attributes != target.attributes {
            lock.unlock()
            flushTyping()
            lock.lock()
        }
        typingTarget = target
        typing.append(characters)
        lock.unlock()
    }

    /// Emit what has been typed into the focused control as one `type` observation.
    private func flushTyping() {
        lock.lock()
        let text = typing
        let target = typingTarget
        typing = ""
        typingTarget = nil
        lock.unlock()
        guard !text.isEmpty, let target, let app = application(target.pid) else { return }
        deliver?(
            Observation(
                kind: .type, bundleId: app.bundleId, appName: app.name, target: target.attributes,
                windowTitle: target.windowTitle, text: text, at: Date()))
    }
}
