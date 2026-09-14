import XCTest
import CoreGraphics
import ApplicationServices

@testable import QuansioMacBridge

/// The accessibility surface, exercised against the live system (EXEC-010).
///
/// These tests do **not** post input events or write the clipboard without restoring it: the machine
/// running them is somebody's, and a test suite that moves the pointer or clobbers the pasteboard to prove
/// a function exists has damaged the thing it was measuring. What they do instead is drive the parts whose
/// effect is a *read*: identity, a bounded tree walk, and clipboard round-trip with the previous value put
/// back.
final class ComputerUseTests: XCTestCase {
    /// Whether this process may use AX at all. It is a TCC permission an operator grants, so the tests that
    /// need it say so rather than assuming it.
    private var trusted: Bool { automationTrusted() }

    func testFrontmostAppResolvesWithoutAccessibility() throws {
        // Identity comes from the workspace, so it resolves even when AX is not granted.
        let app = try frontmostApp()
        XCTAssertGreaterThan(app.pid, 0)
        XCTAssertFalse(app.name.isEmpty, "the frontmost application has no name")
    }

    func testATreeWalkIsBounded() throws {
        guard trusted else {
            throw XCTSkip("Accessibility is not granted to this process, so no tree can be read")
        }
        let app = try frontmostApp()
        // One node is one node, whatever the application's tree looks like.
        let single = try boundedTree(pid: app.pid, maxNodes: 1, maxDepth: 8)
        XCTAssertEqual(single.nodes.count, 1)
        XCTAssertEqual(single.nodes[0].role, "AXApplication")
        XCTAssertEqual(single.app.pid, app.pid)

        // A depth bound stops the walk rather than being ignored.
        let shallow = try boundedTree(pid: app.pid, maxNodes: 512, maxDepth: 0)
        XCTAssertEqual(shallow.nodes.count, 1)
        XCTAssertEqual(shallow.nodes[0].depth, 0)

        // A usable read of a real application returns a role for every node it reports.
        let tree = try boundedTree(pid: app.pid, maxNodes: 64, maxDepth: 4)
        XCTAssertFalse(tree.nodes.isEmpty)
        XCTAssertLessThanOrEqual(tree.nodes.count, 64)
        for node in tree.nodes {
            XCTAssertFalse(node.role.isEmpty, "a node was reported with no role: \(node)")
            XCTAssertGreaterThanOrEqual(node.depth, 0)
        }
    }

    func testBoundsThatAreNotUsableAreRefused() throws {
        guard trusted else {
            throw XCTSkip("Accessibility is not granted to this process")
        }
        let app = try frontmostApp()
        XCTAssertThrowsError(try boundedTree(pid: app.pid, maxNodes: 0, maxDepth: 4)) { error in
            XCTAssertEqual(error as? ComputerUseError, .invalidBound("maxNodes must be > 0 and maxDepth >= 0"))
        }
        XCTAssertThrowsError(try boundedTree(pid: app.pid, maxNodes: 16, maxDepth: -1)) { error in
            XCTAssertEqual(error as? ComputerUseError, .invalidBound("maxNodes must be > 0 and maxDepth >= 0"))
        }
        // A pid that is not an application is a typed refusal rather than a panic.
        XCTAssertThrowsError(try boundedTree(pid: 0, maxNodes: 8, maxDepth: 2))
    }

    func testTheClipboardRoundTripsAndIsPutBack() throws {
        let previous = clipboardText()
        defer { setClipboardText(previous) }
        let marker = "quansio-bridge-test-\(UUID().uuidString)"
        setClipboardText(marker)
        XCTAssertEqual(clipboardText(), marker)
        setClipboardText(previous)
        XCTAssertEqual(clipboardText(), previous)
    }

    func testScreenCaptureAvailabilityAgreesWithReadableWindowTitles() {
        // Screen Recording is what makes a window's title readable, so the two answers must agree.
        let windows = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]] ?? []
        let anyTitled = windows.contains { ($0[kCGWindowName as String] as? String)?.isEmpty == false }
        XCTAssertEqual(screenCaptureAvailable(), anyTitled)
    }
}
