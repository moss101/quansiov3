import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers
import XCTest

@testable import QuansioMacBridge

/// A clock a test moves by hand.
final class ManualClock: RecorderClock, @unchecked Sendable {
    private let lock = NSLock()
    private var current: Date

    init(_ start: Date = ManualClock.epoch) { current = start }

    static let epoch = Date(timeIntervalSince1970: 1_791_108_000)  // 2026-10-04T10:00:00Z

    func now() -> Date {
        lock.lock()
        defer { lock.unlock() }
        return current
    }

    func advance(seconds: Double) {
        lock.lock()
        current = current.addingTimeInterval(seconds)
        lock.unlock()
    }
}

/// A secure-input answer a test sets by hand (the real answer has its own live test).
final class ScriptedSecureInput: SecureInputProbe, @unchecked Sendable {
    private let lock = NSLock()
    private var active = false

    func set(_ value: Bool) {
        lock.lock()
        active = value
        lock.unlock()
    }

    func isSecureInputActive() -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return active
    }
}

struct FixedRecognizer: TextRecognizer {
    let text: String
    func recognizeText(inPNG data: Data) throws -> String { text }
}

let testOwner = RecordingOwner(tenantId: "tn_01J8Z3K6F1N8VQ2X5W9YADP003", userId: "usr_01J8Z3K6F1N8VQ2X5W9YADP003", epoch: 3)
let testRecording = "rec_01J8Z3K6F1N8VQ2X5W9YADP003"

let notes = (bundle: "com.apple.Notes", name: "Notes")

func observation(
    _ kind: RecordedActionKind, at clock: ManualClock, role: String = "AXButton", title: String? = nil,
    identifier: String? = nil, text: String? = nil, key: String? = nil, subrole: String? = nil,
    window: String? = nil
) -> Observation {
    Observation(
        kind: kind, bundleId: notes.bundle, appName: notes.name,
        target: FieldAttributes(role: role, subrole: subrole, title: title, identifier: identifier),
        windowTitle: window, text: text, key: key, at: clock.now())
}

/// A real PNG: a solid colour, optionally with text drawn on it, made with CoreGraphics.
func makePNG(width: Int, height: Int, red: CGFloat = 0.2, green: CGFloat = 0.4, blue: CGFloat = 0.8, text: String? = nil)
    -> Data
{
    let space = CGColorSpaceCreateDeviceRGB()
    let context = CGContext(
        data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0, space: space,
        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    context.setFillColor(CGColor(red: red, green: green, blue: blue, alpha: 1))
    context.fill(CGRect(x: 0, y: 0, width: width, height: height))
    if let text {
        context.setFillColor(CGColor(red: 1, green: 1, blue: 1, alpha: 1))
        context.setFillColor(CGColor(gray: 1, alpha: 1))
        context.fill(CGRect(x: 0, y: 0, width: width, height: height))
        drawText(text, in: context, width: width, height: height)
    }
    let image = context.makeImage()!
    let output = NSMutableData()
    let destination = CGImageDestinationCreateWithData(output, UTType.png.identifier as CFString, 1, nil)!
    CGImageDestinationAddImage(destination, image, nil)
    CGImageDestinationFinalize(destination)
    return output as Data
}

import AppKit

private func drawText(_ text: String, in context: CGContext, width: Int, height: Int) {
    let graphics = NSGraphicsContext(cgContext: context, flipped: false)
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = graphics
    let attributes: [NSAttributedString.Key: Any] = [
        .font: NSFont.systemFont(ofSize: 64, weight: .medium), .foregroundColor: NSColor.black,
    ]
    NSAttributedString(string: text, attributes: attributes).draw(at: CGPoint(x: 24, y: CGFloat(height) / 2 - 32))
    NSGraphicsContext.restoreGraphicsState()
}

/// The repository root, from this file's own path.
var repositoryRoot: URL {
    URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent()  // QuansioMacBridgeTests
        .deletingLastPathComponent()  // Tests
        .deletingLastPathComponent()  // macos
        .deletingLastPathComponent()  // native
        .deletingLastPathComponent()  // repository
}

func jsonObject(_ data: Data) throws -> [String: Any] {
    try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
}
