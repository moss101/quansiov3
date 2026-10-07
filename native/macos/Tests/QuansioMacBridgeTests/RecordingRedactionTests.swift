import XCTest

@testable import QuansioMacBridge

/// Redaction rule set `v1` as the recorder applies it (ADP-003, DOMAIN.md 11.7 C).
///
/// The hand-written corpus is the contract the recorder, the server and the plane share
/// (tests/contract/fixtures/recording/redaction_corpus.json): this file proves the recorder agrees with every
/// case, that redaction is idempotent, and that a summary is counts and never values.
final class RecordingRedactionTests: XCTestCase {
    private func corpus() throws -> [String: Any] {
        let url = repositoryRoot.appendingPathComponent("tests/contract/fixtures/recording/redaction_corpus.json")
        return try jsonObject(try Data(contentsOf: url))
    }

    private func attributes(_ field: [String: Any]) -> FieldAttributes {
        FieldAttributes(
            role: field["ax_role"] as? String, subrole: field["ax_subrole"] as? String,
            title: field["ax_title"] as? String, identifier: field["ax_identifier"] as? String,
            descriptionText: field["ax_description"] as? String)
    }

    func testTheCorpusIsTheContract() throws {
        let corpus = try corpus()
        XCTAssertEqual(corpus["rule_set"] as? String, recordingRuleSet)
        XCTAssertEqual(corpus["categories"] as? [String], RedactionCategory.allCases.map(\.rawValue))
        let cases = try XCTUnwrap(corpus["cases"] as? [[String: Any]])
        XCTAssertGreaterThanOrEqual(cases.count, 40)
        XCTAssertEqual(Set(cases.compactMap { $0["id"] as? String }).count, cases.count)
    }

    func testTheRecorderAgreesWithEveryCorpusCase() throws {
        let cases = try XCTUnwrap(try corpus()["cases"] as? [[String: Any]])
        for entry in cases {
            let id = try XCTUnwrap(entry["id"] as? String)
            let input = try XCTUnwrap(entry["input"] as? String)
            let expected = entry["redacted"] as? String  // null (NSNull) means dropped
            let expectedCounts = RedactionCounts(
                (entry["counts"] as? [String: Int] ?? [:]).reduce(into: [:]) { result, pair in
                    if let category = RedactionCategory(rawValue: pair.key) { result[category] = pair.value }
                })
            let outcome: (text: String?, counts: RedactionCounts)
            if let field = entry["field"] as? [String: Any] {
                outcome = redactFieldText(attributes(field), input)
            } else {
                let plain = redactText(input)
                outcome = (plain.text, plain.counts)
            }
            XCTAssertEqual(outcome.text, expected, "\(id): redacted text")
            XCTAssertEqual(outcome.counts, expectedCounts, "\(id): counts")
            if let text = outcome.text {
                XCTAssertEqual(redactText(text).text, text, "\(id): not idempotent")
                XCTAssertTrue(scanText(text).isEmpty, "\(id): a scan of the output finds more")
            }
        }
    }

    func testASummaryIsCountsAndNeverAValue() {
        let result = redactText("AKIAIOSFODNN7EXAMPLE and card 4111 1111 1111 1111 and Bearer abcdefgh12345678")
        XCTAssertEqual(result.counts[.apiKey], 1)
        XCTAssertEqual(result.counts[.cardNumber], 1)
        XCTAssertEqual(result.counts[.bearerToken], 1)
        let summary = result.counts.summary
        XCTAssertEqual(Set(summary.keys), Set(RedactionCategory.allCases.map(\.rawValue)))
        XCTAssertFalse("\(summary)".contains("AKIA") || "\(summary)".contains("4111"))
        XCTAssertFalse(result.text.contains("AKIA") || result.text.contains("4111") || result.text.contains("abcdefgh"))
    }

    func testLabelsAreReadAsWordsNotSubstrings() {
        XCTAssertEqual(fieldRule(FieldAttributes(identifier: "passwordField")), .labelled(.passwordField))
        XCTAssertNil(fieldRule(FieldAttributes(title: "Footprint")), "otp inside a word is not a code field")
        XCTAssertEqual(fieldRule(FieldAttributes(role: "AXSecureTextField")), .secure)
    }
}
