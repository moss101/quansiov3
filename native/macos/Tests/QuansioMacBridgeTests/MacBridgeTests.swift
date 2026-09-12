import XCTest

@testable import QuansioMacBridge

final class MacBridgeTests: XCTestCase {
    func testAppleSiliconIsSupported() {
        XCTAssertEqual(capsuleSubstrate(architecture: "arm64"), .appleVirtualization)
    }

    func testIntelIsBestEffort() {
        XCTAssertEqual(capsuleSubstrate(architecture: "x86_64"), .bestEffortIntel)
    }

    func testUnknownArchitectureFailsClosed() throws {
        XCTAssertEqual(capsuleSubstrate(architecture: "riscv64"), .unsupported)
        XCTAssertThrowsError(try requireCapsuleSupport(architecture: "riscv64")) { error in
            XCTAssertEqual(error as? MacBridgeError, .capsuleUnsupported("no local capsule substrate for riscv64"))
        }
    }

    func testRequireSupportPassesOnArm64() throws {
        try requireCapsuleSupport(architecture: "arm64")
        try requireCapsuleSupport(architecture: "x86_64")
    }
}
