// macOS Virtualization.framework capsule bridge (EXEC-003).
//
// Rust owns target lifecycle, generation fences, image verification, policy and effects. This file
// performs only the platform operations Rust cannot: configure a Linux VM, attach one private
// virtio-socket device, and start/stop/save/restore it. Deliberately absent are network devices,
// directory shares, Keychain access and arbitrary host ports.

import Foundation
import Virtualization

/// The one qworkerd port compiled into the signed guest image and Rust controller.
public let qworkerdVSOCKPort: UInt32 = 40_581

/// Files already digest-verified by the Rust machine authority.
public struct MacOSCapsuleSpec: Sendable {
    public let kernelURL: URL
    public let initialRamdiskURL: URL?
    public let rootDiskURL: URL
    public let overlayDiskURL: URL
    public let checkpointURL: URL
    public let cpuCount: Int
    public let memorySize: UInt64
    public let overlaySize: UInt64
    public let qworkerdPort: UInt32

    public init(
        kernelURL: URL,
        initialRamdiskURL: URL? = nil,
        rootDiskURL: URL,
        overlayDiskURL: URL,
        checkpointURL: URL,
        cpuCount: Int,
        memorySize: UInt64,
        overlaySize: UInt64,
        qworkerdPort: UInt32 = qworkerdVSOCKPort
    ) {
        self.kernelURL = kernelURL
        self.initialRamdiskURL = initialRamdiskURL
        self.rootDiskURL = rootDiskURL
        self.overlayDiskURL = overlayDiskURL
        self.checkpointURL = checkpointURL
        self.cpuCount = cpuCount
        self.memorySize = memorySize
        self.overlaySize = overlaySize
        self.qworkerdPort = qworkerdPort
    }
}

/// Observable isolation properties used by the Rust adapter and tests.
public struct MacOSCapsuleIsolation: Equatable, Sendable {
    public let networkDeviceCount: Int
    public let directoryShareCount: Int
    public let socketDeviceCount: Int
    public let rootDiskReadOnly: Bool
    public let overlayDiskReadOnly: Bool
}

/// Fail-closed native bridge errors.
public enum MacOSCapsuleError: Error, Equatable {
    case virtualizationUnsupported
    case invalidPrivatePort(UInt32)
    case artifactIsNotAFileURL(String)
    case invalidOverlaySize(UInt64)
    case invalidConfiguration(String)
    case invalidState(String)
    case checkpointUnsupported
    case qworkerdChannelUnavailable
}

/// Create the ephemeral raw data disk. Existing bytes are never reused by reset.
public func recreateCapsuleOverlay(at url: URL, size: UInt64) throws {
    guard url.isFileURL else { throw MacOSCapsuleError.artifactIsNotAFileURL(url.absoluteString) }
    guard size > 0, size % 512 == 0 else { throw MacOSCapsuleError.invalidOverlaySize(size) }
    let manager = FileManager.default
    if manager.fileExists(atPath: url.path) {
        try manager.removeItem(at: url)
    }
    try manager.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
    guard manager.createFile(atPath: url.path, contents: nil) else {
        throw MacOSCapsuleError.invalidConfiguration("cannot create writable overlay")
    }
    let handle = try FileHandle(forWritingTo: url)
    try handle.truncate(atOffset: size)
    try handle.close()
}

/// Build a Linux VM with a read-only root disk, ephemeral data disk and private qworkerd socket.
///
/// `validate` exists solely so unit tests can inspect the device graph using tiny fixture files.
/// Production callers must keep the default `true` and therefore cross the real entitlement and
/// Virtualization.framework validation boundary.
public func makeMacOSCapsuleConfiguration(
    _ spec: MacOSCapsuleSpec,
    validate: Bool = true
) throws -> VZVirtualMachineConfiguration {
    guard VZVirtualMachine.isSupported else { throw MacOSCapsuleError.virtualizationUnsupported }
    guard spec.qworkerdPort == qworkerdVSOCKPort else {
        throw MacOSCapsuleError.invalidPrivatePort(spec.qworkerdPort)
    }
    for url in [spec.kernelURL, spec.rootDiskURL, spec.overlayDiskURL] {
        guard url.isFileURL else { throw MacOSCapsuleError.artifactIsNotAFileURL(url.absoluteString) }
    }
    if let initrd = spec.initialRamdiskURL, !initrd.isFileURL {
        throw MacOSCapsuleError.artifactIsNotAFileURL(initrd.absoluteString)
    }

    let bootLoader = VZLinuxBootLoader(kernelURL: spec.kernelURL)
    bootLoader.initialRamdiskURL = spec.initialRamdiskURL
    // The immutable image launches qworkerd as PID 1 and uses the second disk for writable state.
    // There is no IP interface; all permitted egress is proxied by the host over the typed channel.
    bootLoader.commandLine = "console=hvc0 root=/dev/vda ro quansio.overlay=/dev/vdb quansio.qworkerd.vsock=40581"

    let rootAttachment = try VZDiskImageStorageDeviceAttachment(url: spec.rootDiskURL, readOnly: true)
    let overlayAttachment = try VZDiskImageStorageDeviceAttachment(url: spec.overlayDiskURL, readOnly: false)

    let configuration = VZVirtualMachineConfiguration()
    configuration.bootLoader = bootLoader
    configuration.cpuCount = spec.cpuCount
    configuration.memorySize = spec.memorySize
    configuration.storageDevices = [
        VZVirtioBlockDeviceConfiguration(attachment: rootAttachment),
        VZVirtioBlockDeviceConfiguration(attachment: overlayAttachment),
    ]
    configuration.entropyDevices = [VZVirtioEntropyDeviceConfiguration()]
    configuration.socketDevices = [VZVirtioSocketDeviceConfiguration()]
    configuration.networkDevices = []
    configuration.directorySharingDevices = []

    if validate {
        do {
            try configuration.validate()
        } catch {
            throw MacOSCapsuleError.invalidConfiguration(String(describing: error))
        }
    }
    return configuration
}

/// Summarize the security-relevant device graph without exposing mutable configuration.
public func capsuleIsolation(_ configuration: VZVirtualMachineConfiguration) -> MacOSCapsuleIsolation {
    let root = configuration.storageDevices.first as? VZVirtioBlockDeviceConfiguration
    let overlay = configuration.storageDevices.dropFirst().first as? VZVirtioBlockDeviceConfiguration
    let rootAttachment = root?.attachment as? VZDiskImageStorageDeviceAttachment
    let overlayAttachment = overlay?.attachment as? VZDiskImageStorageDeviceAttachment
    return MacOSCapsuleIsolation(
        networkDeviceCount: configuration.networkDevices.count,
        directoryShareCount: configuration.directorySharingDevices.count,
        socketDeviceCount: configuration.socketDevices.count,
        rootDiskReadOnly: rootAttachment?.isReadOnly ?? false,
        overlayDiskReadOnly: overlayAttachment?.isReadOnly ?? true
    )
}

/// Minimal lifecycle wrapper. Every call is serialized on the VZVirtualMachine queue.
public final class MacOSCapsuleMachine: @unchecked Sendable {
    private let queue: DispatchQueue
    private var spec: MacOSCapsuleSpec
    private var configuration: VZVirtualMachineConfiguration
    private var machine: VZVirtualMachine

    public init(spec: MacOSCapsuleSpec) throws {
        self.spec = spec
        self.queue = DispatchQueue(label: "com.quansio.macos-capsule")
        try recreateCapsuleOverlay(at: spec.overlayDiskURL, size: spec.overlaySize)
        let configuration = try makeMacOSCapsuleConfiguration(spec)
        self.configuration = configuration
        self.machine = VZVirtualMachine(configuration: configuration, queue: queue)
    }

    /// The native observation; Rust maps this into ExecutionTarget observed state.
    public var state: VZVirtualMachine.State {
        queue.sync { machine.state }
    }

    /// Security-relevant configured devices.
    public var isolation: MacOSCapsuleIsolation {
        queue.sync { capsuleIsolation(configuration) }
    }

    /// Start once. A duplicate call after the VM is running is a successful replay.
    public func start() async throws {
        try await withCheckedThrowingContinuation { continuation in
            queue.async {
                if self.machine.state == .running {
                    continuation.resume()
                } else if !self.machine.canStart {
                    continuation.resume(throwing: MacOSCapsuleError.invalidState("cannot start from \(self.machine.state.rawValue)"))
                } else {
                    self.machine.start { result in continuation.resume(with: result) }
                }
            }
        }
    }

    /// Stop once. A duplicate call after stop is a successful replay.
    public func stop() async throws {
        try await withCheckedThrowingContinuation { continuation in
            queue.async {
                if self.machine.state == .stopped {
                    continuation.resume()
                } else if !self.machine.canStop {
                    continuation.resume(throwing: MacOSCapsuleError.invalidState("cannot stop from \(self.machine.state.rawValue)"))
                } else {
                    self.machine.stop { error in
                        if let error { continuation.resume(throwing: error) }
                        else { continuation.resume() }
                    }
                }
            }
        }
    }

    /// Discard only the writable disk, rebuild the same validated device graph and start it.
    public func reset() async throws {
        try await stop()
        try recreateCapsuleOverlay(at: spec.overlayDiskURL, size: spec.overlaySize)
        let configuration = try makeMacOSCapsuleConfiguration(spec)
        queue.sync {
            self.configuration = configuration
            machine = VZVirtualMachine(configuration: configuration, queue: queue)
        }
        try await start()
    }

    /// Connect only to the fixed qworkerd listener. No API accepts an arbitrary port.
    public func connectQworkerd() async throws -> VZVirtioSocketConnection {
        try await withCheckedThrowingContinuation { continuation in
            queue.async {
                guard let socket = self.machine.socketDevices.first as? VZVirtioSocketDevice else {
                    continuation.resume(throwing: MacOSCapsuleError.qworkerdChannelUnavailable)
                    return
                }
                socket.connect(toPort: qworkerdVSOCKPort) { result in continuation.resume(with: result) }
            }
        }
    }

    #if arch(arm64)
    /// Pause, save host-bound machine state, and resume the guest.
    public func checkpoint() async throws {
        try await pause()
        do {
            try await saveState()
            try await resume()
        } catch {
            try? await resume()
            throw error
        }
    }

    /// Restore the stopped VM and resume the paused restored state.
    public func restore() async throws {
        try await stop()
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            queue.async {
                self.machine.restoreMachineStateFrom(url: self.spec.checkpointURL) { error in
                    if let error { continuation.resume(throwing: error) }
                    else { continuation.resume() }
                }
            }
        }
        try await resume()
    }

    private func pause() async throws {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            queue.async {
                guard self.machine.canPause else {
                    continuation.resume(throwing: MacOSCapsuleError.invalidState("VM cannot pause"))
                    return
                }
                self.machine.pause { result in continuation.resume(with: result) }
            }
        }
    }

    private func resume() async throws {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            queue.async {
                guard self.machine.canResume else {
                    continuation.resume(throwing: MacOSCapsuleError.invalidState("VM cannot resume"))
                    return
                }
                self.machine.resume { result in continuation.resume(with: result) }
            }
        }
    }

    private func saveState() async throws {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            queue.async {
                self.machine.saveMachineStateTo(url: self.spec.checkpointURL) { error in
                    if let error { continuation.resume(throwing: error) }
                    else { continuation.resume() }
                }
            }
        }
    }
    #else
    public func checkpoint() async throws { throw MacOSCapsuleError.checkpointUnsupported }
    public func restore() async throws { throw MacOSCapsuleError.checkpointUnsupported }
    #endif
}
