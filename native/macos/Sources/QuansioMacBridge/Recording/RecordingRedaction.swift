// The recording redaction rule set `v1` (ADP-003, DOMAIN.md 11.7 C), as the recorder applies it.
//
// Redaction runs here, in the person's own process, *before* anything is buffered into an archive: an
// action's typed text, a transcript segment, the text recognised on a screenshot. Nothing this file removes
// is ever stored or sent. The server and the intelligence plane run the same rule set again on what arrives;
// all three are pinned to one hand-written corpus (tests/contract/fixtures/recording/redaction_corpus.json),
// and the patterns use only the vocabulary the three engines read the same way (no lookaround, case folding
// only), in the same order, with the same markers and the same field rules.

import Foundation

/// What redaction removes, in the order the rules run.
public enum RedactionCategory: String, CaseIterable, Codable, Sendable, Comparable {
    case apiKey = "api_key"
    case bearerToken = "bearer_token"
    case cardNumber = "card_number"
    case verificationCode = "verification_code"
    case passwordField = "password_field"

    /// What replaces a removed value.
    public var marker: String { "[REDACTED:\(rawValue)]" }

    public static func < (lhs: RedactionCategory, rhs: RedactionCategory) -> Bool {
        allCases.firstIndex(of: lhs)! < allCases.firstIndex(of: rhs)!
    }
}

/// The rule-set version an archive declares.
public let recordingRuleSet = "v1"

/// How many values of each category a text held. Counts, never values.
public struct RedactionCounts: Equatable, Sendable {
    public private(set) var byCategory: [RedactionCategory: Int] = [:]

    public init() {}

    public init(_ counts: [RedactionCategory: Int]) {
        for (category, n) in counts where n > 0 { byCategory[category] = n }
    }

    public mutating func add(_ category: RedactionCategory, _ n: Int = 1) {
        guard n > 0 else { return }
        byCategory[category, default: 0] += n
    }

    public mutating func merge(_ other: RedactionCounts) {
        for (category, n) in other.byCategory { add(category, n) }
    }

    public subscript(category: RedactionCategory) -> Int { byCategory[category] ?? 0 }

    public var isEmpty: Bool { byCategory.values.allSatisfy { $0 == 0 } }

    /// The summary object: every category present, zero included.
    public var summary: [String: Int] {
        Dictionary(uniqueKeysWithValues: RedactionCategory.allCases.map { ($0.rawValue, self[$0]) })
    }
}

private func regex(_ pattern: String, caseInsensitive: Bool = false) -> NSRegularExpression {
    do {
        return try NSRegularExpression(pattern: pattern, options: caseInsensitive ? [.caseInsensitive] : [])
    } catch {
        fatalError("redaction pattern \(pattern): \(error)")
    }
}

private let triggers = "(?:otp|passcode|2fa|mfa|pin|code|verification|one[- ]?time|confirmation)"

private let pemBlock = regex("-----BEGIN [A-Z ]*PRIVATE KEY-----[\\s\\S]*?-----END [A-Z ]*PRIVATE KEY-----")
private let pemHeader = regex("-----BEGIN [A-Z ]*PRIVATE KEY-----")
private let apiKeyShapes: [NSRegularExpression] = [
    "\\b(?:sk|pk|rk|whsec)[_-][A-Za-z0-9_-]{16,}",
    "\\b(?:AKIA|ASIA)[0-9A-Z]{16}\\b",
    "\\bgh[pousr]_[A-Za-z0-9]{30,}",
    "\\bgithub_pat_[A-Za-z0-9_]{40,}",
    "\\bxox[abprs]-[A-Za-z0-9-]{10,}",
    "\\bAIza[0-9A-Za-z_-]{35,}",
    "\\beyJ[A-Za-z0-9_-]{8,}\\.[A-Za-z0-9_-]{8,}\\.[A-Za-z0-9_-]{8,}",
].map { regex($0) }
private let longToken = regex("\\b[A-Za-z0-9_-]{40,}\\b")
private let authorization = regex(
    "\\bauthorization\\s*[:=]\\s*(?:(?:bearer|basic|token)\\s+)?[A-Za-z0-9._~+/=-]{8,}", caseInsensitive: true)
private let bearer = regex("\\bbearer\\s+[A-Za-z0-9._~+/=-]{8,}", caseInsensitive: true)
private let card = regex("\\d(?:[ -]?\\d){12,}")
private let codeAfterWord = regex("\\b\(triggers)\\b[^0-9\\n]{0,40}\\b\\d{4,8}\\b", caseInsensitive: true)
private let codeBeforeWord = regex("\\b\\d{4,8}\\b[^0-9\\n]{0,40}\\b\(triggers)\\b", caseInsensitive: true)

private func replaceAll(
    _ text: String,
    _ pattern: NSRegularExpression,
    _ category: RedactionCategory,
    _ counts: inout RedactionCounts,
    keep: ((String) -> Bool)? = nil
) -> String {
    let ns = text as NSString
    let matches = pattern.matches(in: text, range: NSRange(location: 0, length: ns.length))
    guard !matches.isEmpty else { return text }
    let out = NSMutableString(string: text)
    var found = 0
    for match in matches.reversed() {
        if let keep, keep(ns.substring(with: match.range)) { continue }
        out.replaceCharacters(in: match.range, with: category.marker)
        found += 1
    }
    counts.add(category, found)
    return out as String
}

/// Redact free text: the text with every match replaced, and the count per category found.
public func redactText(_ text: String) -> (text: String, counts: RedactionCounts) {
    var counts = RedactionCounts()
    var out = replaceAll(text, pemBlock, .apiKey, &counts)
    out = replaceAll(out, pemHeader, .apiKey, &counts)
    for shape in apiKeyShapes { out = replaceAll(out, shape, .apiKey, &counts) }
    // A long run is a token and not a word only when it holds both a letter and a digit.
    out = replaceAll(out, longToken, .apiKey, &counts) { token in
        let letter = token.unicodeScalars.contains { $0.isASCII && CharacterSet.letters.contains($0) }
        let digit = token.unicodeScalars.contains { $0.isASCII && ("0"..."9").contains($0) }
        return !(letter && digit)
    }
    out = replaceAll(out, authorization, .bearerToken, &counts)
    out = replaceAll(out, bearer, .bearerToken, &counts)
    out = replaceAll(out, card, .cardNumber, &counts)
    out = replaceAll(out, codeAfterWord, .verificationCode, &counts)
    out = replaceAll(out, codeBeforeWord, .verificationCode, &counts)
    return (out, counts)
}

/// What a text still holds that redaction would remove. Empty for any redacted text.
public func scanText(_ text: String) -> RedactionCounts { redactText(text).counts }

/// The accessibility attributes of the control text is entered into.
public struct FieldAttributes: Equatable, Sendable {
    public var role: String?
    public var subrole: String?
    public var title: String?
    public var identifier: String?
    public var descriptionText: String?

    public init(
        role: String? = nil, subrole: String? = nil, title: String? = nil, identifier: String? = nil,
        descriptionText: String? = nil
    ) {
        self.role = role
        self.subrole = subrole
        self.title = title
        self.identifier = identifier
        self.descriptionText = descriptionText
    }
}

/// What entering text into a control means for that text.
public enum FieldRule: Equatable, Sendable {
    /// A secure text field: whatever was typed is dropped whole.
    case secure
    /// A control whose label says what it holds: the text becomes that category's marker.
    case labelled(RedactionCategory)
}

private let secureRole = "AXSecureTextField"

private func labelTokens(_ label: String) -> [String] {
    var spaced = ""
    var previous: Character?
    for char in label {
        if let p = previous, let pa = p.asciiValue, let ca = char.asciiValue,
            (UInt8(ascii: "a")...UInt8(ascii: "z")).contains(pa) || (UInt8(ascii: "0")...UInt8(ascii: "9")).contains(pa),
            (UInt8(ascii: "A")...UInt8(ascii: "Z")).contains(ca)
        {
            spaced.append(" ")
        }
        spaced.append(char)
        previous = char
    }
    var tokens: [String] = []
    var current = ""
    for scalar in spaced.unicodeScalars {
        let isAlnum = scalar.isASCII && (CharacterSet.alphanumerics.contains(scalar))
        if isAlnum {
            current.unicodeScalars.append(scalar)
        } else if !current.isEmpty {
            tokens.append(current.lowercased())
            current = ""
        }
    }
    if !current.isEmpty { tokens.append(current.lowercased()) }
    return tokens
}

private func hasPair(_ tokens: [String], _ pairs: [(String, String)]) -> Bool {
    guard tokens.count > 1 else { return false }
    for index in 0..<(tokens.count - 1) {
        if pairs.contains(where: { $0.0 == tokens[index] && $0.1 == tokens[index + 1] }) { return true }
    }
    return false
}

private func labelCategory(_ tokens: [String]) -> RedactionCategory? {
    if tokens.contains(where: { ["otp", "2fa", "mfa", "verification", "authenticator", "onetime"].contains($0) })
        || hasPair(tokens, [
            ("one", "time"), ("security", "code"), ("auth", "code"), ("authentication", "code"),
            ("confirmation", "code"), ("sms", "code"), ("login", "code"),
        ])
    {
        return .verificationCode
    }
    if tokens.contains(where: { ["cvv", "cvc", "ccnum", "cardnumber"].contains($0) })
        || hasPair(tokens, [("card", "number"), ("card", "num"), ("credit", "card"), ("debit", "card"), ("cc", "number")])
    {
        return .cardNumber
    }
    if tokens.contains("apikey")
        || hasPair(tokens, [("api", "key"), ("api", "token"), ("access", "token"), ("auth", "token"), ("private", "key")])
    {
        return .apiKey
    }
    if tokens.contains(where: { ["password", "passwd", "pwd", "passphrase", "passcode", "secret"].contains($0) }) {
        return .passwordField
    }
    return nil
}

/// What a control's own attributes say about text entered into it, if anything.
public func fieldRule(_ attributes: FieldAttributes) -> FieldRule? {
    if attributes.role == secureRole || attributes.subrole == secureRole { return .secure }
    let labels = [attributes.title, attributes.identifier, attributes.descriptionText].compactMap { $0 }
    let found = Set(labels.compactMap { labelCategory(labelTokens($0)) })
    for category in RedactionCategory.allCases where category != .bearerToken && found.contains(category) {
        return .labelled(category)
    }
    return nil
}

/// Redact text entered into a control: dropped (`nil`) in a secure field, the category marker in a labelled
/// one, otherwise the ordinary text rules. Empty text is nothing and counts nothing.
public func redactFieldText(_ attributes: FieldAttributes, _ text: String) -> (text: String?, counts: RedactionCounts) {
    if text.isEmpty { return ("", RedactionCounts()) }
    switch fieldRule(attributes) {
    case .secure:
        return (nil, RedactionCounts([.passwordField: 1]))
    case .labelled(let category):
        return (category.marker, RedactionCounts([category: 1]))
    case nil:
        let redacted = redactText(text)
        return (redacted.text, redacted.counts)
    }
}
