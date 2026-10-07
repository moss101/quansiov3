// Bounded periodic screenshots with redaction before anything is kept (ADP-003, DOMAIN.md 11.7 C).
//
// Pixels cannot be edited in place, so a screenshot is judged by the text on it: the text is recognised on
// this machine (Vision), run through the same redaction rule set as everything else, and a screenshot whose
// text matches any category is withheld whole and counted. What is kept is bounded in count, spacing, bytes
// and size, and an image identical to the last one kept is not kept twice.

import CoreGraphics
import CryptoKit
import Foundation
import ImageIO
import UniformTypeIdentifiers
import Vision

/// Reads the text off an image.
public protocol TextRecognizer: Sendable {
    func recognizeText(inPNG data: Data) throws -> String
}

/// Vision's on-device text recognition: no network, no permission.
public struct VisionTextRecognizer: TextRecognizer {
    public init() {}

    public func recognizeText(inPNG data: Data) throws -> String {
        guard let source = CGImageSourceCreateWithData(data as CFData, nil),
            let image = CGImageSourceCreateImageAtIndex(source, 0, nil)
        else { return "" }
        let request = VNRecognizeTextRequest()
        request.recognitionLevel = .accurate
        request.usesLanguageCorrection = false
        let handler = VNImageRequestHandler(cgImage: image, options: [:])
        try handler.perform([request])
        let lines = (request.results ?? []).compactMap { $0.topCandidates(1).first?.string }
        return lines.joined(separator: "\n")
    }
}

/// One kept screenshot.
public struct KeptScreenshot: Equatable, Sendable {
    public let at: Date
    public let png: Data
    public let sha256: String
    public let width: Int
    public let height: Int
    public let screenText: String
}

/// What one sampling attempt did.
public enum SampleOutcome: Equatable, Sendable {
    case kept
    /// The previous attempt was less than the minimum interval ago.
    case tooSoon
    /// The count limit is spent.
    case limitReached
    /// Identical to the last screenshot kept.
    case duplicate
    /// Its text matched a redaction rule: the screenshot is withheld whole.
    case withheld
    /// It cannot be made small enough to keep.
    case tooLarge
    /// Screenshots are off, or capture produced nothing.
    case unavailable
}

/// Downscale a PNG so its longer side is at most `maxDimension` pixels, re-encoded as PNG.
public func downscalePNG(_ data: Data, maxDimension: Int) -> (png: Data, width: Int, height: Int)? {
    guard let source = CGImageSourceCreateWithData(data as CFData, nil) else { return nil }
    let options: [CFString: Any] = [
        kCGImageSourceCreateThumbnailFromImageAlways: true,
        kCGImageSourceCreateThumbnailWithTransform: true,
        kCGImageSourceThumbnailMaxPixelSize: maxDimension,
    ]
    guard let image = CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary) else { return nil }
    let output = NSMutableData()
    guard let destination = CGImageDestinationCreateWithData(output, UTType.png.identifier as CFString, 1, nil)
    else { return nil }
    CGImageDestinationAddImage(destination, image, nil)
    guard CGImageDestinationFinalize(destination) else { return nil }
    return (output as Data, image.width, image.height)
}

/// Samples screenshots under the recording's limits. Not thread-safe: the session serializes calls.
public final class ScreenshotSampler {
    public private(set) var kept: [KeptScreenshot] = []
    public private(set) var withheld = 0
    public private(set) var withheldCounts = RedactionCounts()
    private let limits: RecordingLimits
    private let capture: () throws -> Data
    private let recognizer: TextRecognizer
    private var lastAttempt: Date?
    private var lastSha: String?

    public init(limits: RecordingLimits, recognizer: TextRecognizer, capture: @escaping () throws -> Data) {
        self.limits = limits
        self.recognizer = recognizer
        self.capture = capture
    }

    /// Try to take one screenshot at `now`.
    public func sample(at now: Date) -> SampleOutcome {
        guard limits.screenshotsEnabled, limits.screenshotMaxCount > 0 else { return .unavailable }
        if kept.count >= limits.screenshotMaxCount { return .limitReached }
        if let last = lastAttempt, now.timeIntervalSince(last) * 1000 < Double(limits.screenshotMinIntervalMs) {
            return .tooSoon
        }
        lastAttempt = now
        guard let raw = try? capture(), !raw.isEmpty else { return .unavailable }
        var current = raw
        var dimension = limits.screenshotMaxDimensionPx
        var width = 0
        var height = 0
        // Shrink until it fits the byte bound: a screenshot that cannot be made small enough is dropped.
        while true {
            guard let scaled = downscalePNG(current, maxDimension: dimension) else { return .unavailable }
            current = scaled.png
            width = scaled.width
            height = scaled.height
            if current.count <= limits.screenshotMaxBytes { break }
            dimension = dimension * 3 / 4
            if dimension < 64 { return .tooLarge }
            current = raw
        }
        let sha = SHA256.hash(data: current).map { String(format: "%02x", $0) }.joined()
        if sha == lastSha { return .duplicate }
        let recognized = (try? recognizer.recognizeText(inPNG: current)) ?? ""
        let redacted = redactText(recognized)
        if !redacted.counts.isEmpty {
            withheld += 1
            withheldCounts.merge(redacted.counts)
            lastSha = sha
            return .withheld
        }
        let text = String(redacted.text.prefix(limits.screenTextMaxChars))
        kept.append(KeptScreenshot(at: now, png: current, sha256: sha, width: width, height: height, screenText: text))
        lastSha = sha
        return .kept
    }

    /// Drop the oldest kept screenshots to make room (the archive-size bound). Returns whether one was dropped.
    @discardableResult
    public func dropOldest() -> Bool {
        guard !kept.isEmpty else { return false }
        kept.removeFirst()
        return true
    }

    /// Forget everything kept: the owner changed, or the recording was cancelled.
    public func wipe() {
        kept.removeAll()
        withheld = 0
        withheldCounts = RedactionCounts()
        lastSha = nil
        lastAttempt = nil
    }
}

/// The main display as PNG through the real capture API, for the live sampler. Needs Screen Recording.
public func liveScreenCapture(maxBytes: Int = 8 * 1024 * 1024) throws -> Data {
    try boundedScreenshot(maxBytes: maxBytes)
}
