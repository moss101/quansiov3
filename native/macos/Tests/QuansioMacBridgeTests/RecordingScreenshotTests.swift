import XCTest

@testable import QuansioMacBridge

/// Bounded periodic screenshots, de-duplicated and judged by their text (ADP-003, DOMAIN.md 11.7 C).
///
/// The sampler's logic runs on real PNGs made with CoreGraphics. The text-recognition test runs the real
/// Vision framework on an image this test renders: no screen, no permission, the system's own recogniser.
final class RecordingScreenshotTests: XCTestCase {
    private func sampler(
        _ limits: RecordingLimits, text: String = "Notes", capture: @escaping () throws -> Data
    ) -> ScreenshotSampler {
        ScreenshotSampler(limits: limits, recognizer: FixedRecognizer(text: text), capture: capture)
    }

    private let start = ManualClock.epoch

    func testCountSpacingAndDuplicatesAreBounded() throws {
        var limits = RecordingLimits(screenshotMaxCount: 3, screenshotMinIntervalMs: 5000)
        var shot = 0
        var identical = false
        let s = sampler(limits) {
            shot += 1
            return makePNG(width: 80, height: 60, red: identical ? 0.5 : CGFloat(shot) / 10)
        }
        XCTAssertEqual(s.sample(at: start), .kept)
        XCTAssertEqual(s.sample(at: start.addingTimeInterval(2)), .tooSoon, "closer than the minimum interval")
        XCTAssertEqual(shot, 1, "a too-soon attempt does not even capture")
        XCTAssertEqual(s.sample(at: start.addingTimeInterval(6)), .kept)
        identical = true
        XCTAssertEqual(s.sample(at: start.addingTimeInterval(12)), .kept)
        // An identical image is not kept twice.
        XCTAssertEqual(s.sample(at: start.addingTimeInterval(18)), .limitReached, "the count limit is three")
        XCTAssertEqual(s.kept.count, 3)
        limits.screenshotMaxCount = 10
        let again = sampler(limits) { makePNG(width: 80, height: 60) }
        XCTAssertEqual(again.sample(at: start), .kept)
        XCTAssertEqual(again.sample(at: start.addingTimeInterval(10)), .duplicate)
        XCTAssertEqual(again.kept.count, 1)
    }

    func testSizeIsBoundedByDownscalingAndByBytes() throws {
        var limits = RecordingLimits(screenshotMinIntervalMs: 0)
        limits.screenshotMaxDimensionPx = 128
        let s = sampler(limits) { makePNG(width: 1024, height: 768) }
        XCTAssertEqual(s.sample(at: start), .kept)
        let shot = try XCTUnwrap(s.kept.first)
        XCTAssertLessThanOrEqual(max(shot.width, shot.height), 128)

        // A bound no image can meet is a dropped screenshot, never an oversized one.
        limits.screenshotMaxBytes = 16
        let impossible = sampler(limits) { makePNG(width: 400, height: 300, red: 0.9, green: 0.1, blue: 0.5) }
        XCTAssertEqual(impossible.sample(at: start), .tooLarge)
        XCTAssertTrue(impossible.kept.isEmpty)

        limits.screenshotsEnabled = false
        let off = sampler(limits) { XCTFail("captured with screenshots off"); return Data() }
        XCTAssertEqual(off.sample(at: start), .unavailable)
    }

    func testAScreenshotWhoseTextMatchesARuleIsWithheldWholeAndCounted() throws {
        let limits = RecordingLimits(screenshotMinIntervalMs: 0)
        var shot = 0
        let s = sampler(limits, text: "Card 4111 1111 1111 1111 key AKIAIOSFODNN7EXAMPLE") {
            shot += 1
            return makePNG(width: 64, height: 48, red: CGFloat(shot) / 10)
        }
        XCTAssertEqual(s.sample(at: start), .withheld)
        XCTAssertTrue(s.kept.isEmpty)
        XCTAssertEqual(s.withheld, 1)
        XCTAssertEqual(s.withheldCounts[.cardNumber], 1)
        XCTAssertEqual(s.withheldCounts[.apiKey], 1)
        let clean = sampler(limits, text: "Notes  New Note") { makePNG(width: 64, height: 48) }
        XCTAssertEqual(clean.sample(at: start), .kept)
        XCTAssertEqual(clean.kept[0].screenText, "Notes  New Note")
        s.wipe()
        XCTAssertEqual(s.withheld, 0)
    }

    func testScreenTextIsTruncatedAfterRedaction() throws {
        var limits = RecordingLimits(screenshotMinIntervalMs: 0)
        limits.screenTextMaxChars = 12
        let s = sampler(limits, text: "Notes Notes Notes Notes Notes") { makePNG(width: 32, height: 32) }
        XCTAssertEqual(s.sample(at: start), .kept)
        XCTAssertEqual(s.kept[0].screenText, "Notes Notes ")
    }

    /// The real Vision framework on a real rendered image.
    func testTheSystemsOwnRecogniserReadsACardNumberOffAnImageSoItIsWithheld() throws {
        let png = makePNG(width: 1100, height: 220, text: "Card 4111 1111 1111 1111")
        let recognized = try VisionTextRecognizer().recognizeText(inPNG: png)
        let digits = recognized.filter(\.isNumber)
        guard digits.count >= 12 else {
            throw XCTSkip("BLOCKED_EXTERNAL: this host's Vision text recognition did not read the rendered digits (\(recognized.debugDescription))")
        }
        let limits = RecordingLimits(screenshotMinIntervalMs: 0)
        let s = ScreenshotSampler(limits: limits, recognizer: VisionTextRecognizer()) { png }
        XCTAssertEqual(s.sample(at: start), .withheld, "recognised text: \(recognized.debugDescription)")
        XCTAssertTrue(s.kept.isEmpty)
        XCTAssertEqual(s.withheldCounts[.cardNumber], 1)

        let plain = makePNG(width: 700, height: 220, text: "New Note")
        let harmless = ScreenshotSampler(limits: limits, recognizer: VisionTextRecognizer()) { plain }
        XCTAssertEqual(harmless.sample(at: start), .kept)
        XCTAssertTrue(harmless.kept[0].screenText.lowercased().contains("note"), harmless.kept[0].screenText)
    }
}
