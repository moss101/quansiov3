import XCTest

@testable import QuansioMacBridge

/// The action receipt's wire shape and rules (ADP-002).
///
/// Pure encoding and logic: no input is posted and nothing on the machine is touched, so these run
/// everywhere the package builds. The fixtures are the same files the Rust suite parses
/// (`crates/machine/tests/computer_receipts.rs`), so a field renamed on either side fails both.
final class ReceiptTests: XCTestCase {
    private func fixtureURL(_ name: String) -> URL {
        URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()
            .appendingPathComponent("Fixtures")
            .appendingPathComponent(name)
    }

    private func jsonObject(_ data: Data) throws -> NSDictionary {
        let object = try JSONSerialization.jsonObject(with: data)
        return try XCTUnwrap(object as? NSDictionary)
    }

    private let fixtures = [
        "receipt_unverified.json",
        "receipt_applied_background.json",
        "receipt_invariant_violated.json",
        "receipt_refused.json",
    ]

    func testTheFixturesDecodeAndEncodeBackToThemselves() throws {
        for name in fixtures {
            let data = try Data(contentsOf: fixtureURL(name))
            let receipt = try JSONDecoder().decode(ActionReceipt.self, from: data)
            let encoded = try jsonObject(try ReceiptJSON.encode(receipt))
            XCTAssertEqual(encoded, try jsonObject(data), name)
            // What the fixtures say is already what the rules say.
            XCTAssertEqual(receipt.normalized(), receipt, name)
        }
    }

    func testAnUnknownVerdictIsEncodedAsNullAndNeverOmitted() throws {
        let receipt = ActionReceipt.unverified(.hidEvent, .foreground)
        XCTAssertNil(receipt.effectVerified)
        let object = try jsonObject(try ReceiptJSON.encode(receipt))
        XCTAssertTrue(object.allKeys.contains { ($0 as? String) == "effect_verified" })
        XCTAssertEqual(object["effect_verified"] as? NSNull, NSNull())
        XCTAssertEqual(object["outcome"] as? String, "unverified")
        XCTAssertEqual(object["invariants"] as? NSNull, NSNull())
        // True and false are what they are.
        XCTAssertEqual(try jsonObject(try ReceiptJSON.encode(.applied(.pasteboard, .background)))["effect_verified"] as? Bool, true)
        XCTAssertEqual(try jsonObject(try ReceiptJSON.encode(.notApplied(.pasteboard, .background)))["effect_verified"] as? Bool, false)
    }

    func testTheWireWordsAreTheOnesDomainMdStates() throws {
        XCTAssertEqual(ReceiptMethod.accessibilityTree.rawValue, "accessibility_tree")
        XCTAssertEqual(ReceiptMethod.accessibilityAction.rawValue, "accessibility_action")
        XCTAssertEqual(ReceiptMethod.processEvent.rawValue, "process_event")
        XCTAssertEqual(ReceiptMethod.hidEvent.rawValue, "hid_event")
        XCTAssertEqual(ReceiptMethod.pasteboard.rawValue, "pasteboard")
        XCTAssertEqual(ExecutionMethod.background.rawValue, "background")
        XCTAssertEqual(ReceiptOutcome.notApplied.rawValue, "not_applied")
        XCTAssertEqual(ReceiptOutcome.invariantViolated.rawValue, "invariant_violated")
    }

    private func app(_ bundle: String, _ pid: Int32) -> FrontmostApp {
        FrontmostApp(bundleId: bundle, pid: pid)
    }

    private func clean() -> BackgroundInvariants {
        BackgroundInvariants(
            physicalInput: false,
            movedSystemCursor: false,
            frontmostBefore: app("com.apple.TextEdit", 1),
            frontmostAfter: app("com.apple.TextEdit", 1)
        )
    }

    func testABrokenBackgroundClaimOverridesTheObservedEffect() {
        let breakers: [(String, BackgroundInvariants)] = [
            ("physical input", BackgroundInvariants(physicalInput: true, movedSystemCursor: false, frontmostBefore: app("a.b", 1), frontmostAfter: app("a.b", 1))),
            ("cursor", BackgroundInvariants(physicalInput: false, movedSystemCursor: true, frontmostBefore: app("a.b", 1), frontmostAfter: app("a.b", 1))),
            ("frontmost pid", BackgroundInvariants(physicalInput: false, movedSystemCursor: false, frontmostBefore: app("a.b", 1), frontmostAfter: app("a.b", 2))),
            ("frontmost bundle", BackgroundInvariants(physicalInput: false, movedSystemCursor: false, frontmostBefore: app("a.b", 1), frontmostAfter: app("c.d", 1))),
        ]
        for (name, invariants) in breakers {
            XCTAssertTrue(invariants.violated, name)
            let receipt = ActionReceipt.applied(.processEvent, .background).measured(invariants)
            XCTAssertTrue(receipt.invariantViolated, name)
            XCTAssertEqual(receipt.effectVerified, false, name)
            XCTAssertEqual(receipt.outcome, .invariantViolated, name)
            XCTAssertTrue(receipt.dispatched, name)
        }
    }

    func testACleanMeasurementKeepsAVerifiedActionVerifiedAndNoMeasurementLowersIt() {
        let kept = ActionReceipt.applied(.pasteboard, .background).measured(clean())
        XCTAssertEqual(kept.effectVerified, true)
        XCTAssertEqual(kept.outcome, .applied)
        XCTAssertFalse(kept.invariantViolated)

        // Nothing measured: the claim is unproven, so the verification resting on it is unknown.
        let lowered = ActionReceipt.applied(.processEvent, .background).measured(nil)
        XCTAssertNil(lowered.effectVerified)
        XCTAssertEqual(lowered.outcome, .unverified)

        // A foreground action is not subject to the background rules.
        let foreground = ActionReceipt.applied(.hidEvent, .foreground).normalized()
        XCTAssertEqual(foreground.effectVerified, true)
    }

    func testAnActionThatDispatchedNothingIsNotTouchedByTheBackgroundRules() {
        let refused = ActionReceipt.refused(.accessibilityAction, .background)
        XCTAssertEqual(refused.normalized(), refused)
        XCTAssertFalse(refused.dispatched)
        XCTAssertEqual(refused.effectVerified, false)
    }
}
