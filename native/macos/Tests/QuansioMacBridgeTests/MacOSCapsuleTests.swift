import Foundation
import Virtualization
import XCTest

@testable import QuansioMacBridge

/// EXEC-003 native capsule tests.
///
/// Real-boundary variables:
/// - `QUANSIO_TEST_MACOS_VM=1` enables an actual Virtualization.framework boot.
/// - `QUANSIO_TEST_MACOS_GUEST_KERNEL` is an absolute path to the digest-verified Linux kernel.
/// - `QUANSIO_TEST_MACOS_GUEST_ROOT_DISK` is an absolute path to the digest-verified raw root disk.
/// - `QUANSIO_TEST_MACOS_GUEST_INITRD` optionally names the digest-verified initrd.
///
/// The release image must launch qworkerd on virtio-socket port 40581. Missing artifacts are an
/// explicit external blocker, never a simulated pass.
final class MacOSCapsuleTests: XCTestCase {
    private func scratch(_ name: String, size: Int = 512) throws -> URL {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("quansio-exec-003-tests", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let url = directory.appendingPathComponent(name)
        let bytes = Data(repeating: 0x51, count: size)
        try bytes.write(to: url, options: .atomic)
        return url
    }

    private func fixtureSpec(port: UInt32 = qworkerdVSOCKPort) throws -> MacOSCapsuleSpec {
        MacOSCapsuleSpec(
            kernelURL: try scratch("kernel"),
            rootDiskURL: try scratch("root.raw"),
            overlayDiskURL: try scratch("overlay.raw"),
            checkpointURL: try scratch("checkpoint.vzstate"),
            cpuCount: 2,
            memorySize: 2 * 1024 * 1024 * 1024,
            overlaySize: 4096,
            qworkerdPort: port
        )
    }

    func testDeviceGraphHasOnlyPrivateControlAndNoHostOrIPReachability() throws {
        let configuration = try makeMacOSCapsuleConfiguration(fixtureSpec(), validate: false)
        XCTAssertEqual(
            capsuleIsolation(configuration),
            MacOSCapsuleIsolation(
                networkDeviceCount: 0,
                directoryShareCount: 0,
                socketDeviceCount: 1,
                rootDiskReadOnly: true,
                overlayDiskReadOnly: false
            )
        )
    }

    func testArbitraryGuestPortAndHostURLAreDenied() throws {
        XCTAssertThrowsError(
            try makeMacOSCapsuleConfiguration(fixtureSpec(port: 22), validate: false)
        ) { error in
            XCTAssertEqual(error as? MacOSCapsuleError, .invalidPrivatePort(22))
        }

        var spec = try fixtureSpec()
        spec = MacOSCapsuleSpec(
            kernelURL: URL(string: "https://metadata.invalid/kernel")!,
            rootDiskURL: spec.rootDiskURL,
            overlayDiskURL: spec.overlayDiskURL,
            checkpointURL: spec.checkpointURL,
            cpuCount: spec.cpuCount,
            memorySize: spec.memorySize,
            overlaySize: spec.overlaySize
        )
        XCTAssertThrowsError(try makeMacOSCapsuleConfiguration(spec, validate: false)) { error in
            XCTAssertEqual(
                error as? MacOSCapsuleError,
                .artifactIsNotAFileURL("https://metadata.invalid/kernel")
            )
        }
    }

    func testResetRecreatesTheWritableDiskAndPreservesImmutableRoot() throws {
        let root = try scratch("reset-root.raw", size: 4096)
        let overlay = try scratch("reset-overlay.raw", size: 4096)
        let rootBefore = try Data(contentsOf: root)
        try Data(repeating: 0xA5, count: 4096).write(to: overlay)

        try recreateCapsuleOverlay(at: overlay, size: 4096)

        XCTAssertEqual(try Data(contentsOf: root), rootBefore)
        XCTAssertEqual(try Data(contentsOf: overlay), Data(repeating: 0, count: 4096))
    }

    func testRealVMStartStopResetCheckpointAndPrivateQworkerdChannel() async throws {
        let environment = ProcessInfo.processInfo.environment
        guard environment["QUANSIO_TEST_MACOS_VM"] == "1" else {
            throw XCTSkip("BLOCKED_EXTERNAL: QUANSIO_TEST_MACOS_VM=1 is required for the real macOS VM boundary")
        }
        guard let kernel = environment["QUANSIO_TEST_MACOS_GUEST_KERNEL"],
              let root = environment["QUANSIO_TEST_MACOS_GUEST_ROOT_DISK"] else {
            throw XCTSkip(
                "BLOCKED_EXTERNAL: QUANSIO_TEST_MACOS_GUEST_KERNEL and QUANSIO_TEST_MACOS_GUEST_ROOT_DISK must name the signed V8.1 image"
            )
        }
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("quansio-exec-003-live-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let initrd = environment["QUANSIO_TEST_MACOS_GUEST_INITRD"].map { URL(fileURLWithPath: $0) }
        let spec = MacOSCapsuleSpec(
            kernelURL: URL(fileURLWithPath: kernel),
            initialRamdiskURL: initrd,
            rootDiskURL: URL(fileURLWithPath: root),
            overlayDiskURL: directory.appendingPathComponent("overlay.raw"),
            checkpointURL: directory.appendingPathComponent("machine.vzstate"),
            cpuCount: 2,
            memorySize: 2 * 1024 * 1024 * 1024,
            overlaySize: 2 * 1024 * 1024 * 1024
        )
        let machine = try MacOSCapsuleMachine(spec: spec)
        XCTAssertEqual(machine.isolation.networkDeviceCount, 0)
        XCTAssertEqual(machine.isolation.directoryShareCount, 0)

        try await machine.start()
        _ = try await machine.connectQworkerd()
        try await machine.checkpoint()
        try await machine.stop()
        try await machine.restore()
        try await machine.stop()
        try await machine.reset()
        _ = try await machine.connectQworkerd()
        try await machine.stop()
    }
}
