/// macOS native bridge surface.
///
/// Canonical owner: `native/macos`. The Rust machine module is the only caller
/// (DOSSIER.md §5 "Native computer-use: native broker called by Rust machine module").
/// This target must stay narrow: platform capability reporting, Virtualization.framework
/// capsule lifecycle, Accessibility (AX) inspection and Keychain access. It never
/// decides capability, policy, approval or effect outcomes.
public enum MacBridgeError: Error, Equatable {
    /// The host cannot run a local capsule (Apple Silicon is primary for the capsule).
    case capsuleUnsupported(String)
}

/// Substrate support of this host, mirroring DOSSIER.md §21.1.
public enum CapsuleSubstrate: String, Sendable {
    case appleVirtualization
    case bestEffortIntel
    case unsupported
}

/// Report the local-capsule substrate for the running architecture.
///
/// Apple Silicon hosts (arm64) are supported; Intel hosts are best-effort; any other
/// architecture is unsupported and must fail closed.
public func capsuleSubstrate(architecture: String = currentArchitecture()) -> CapsuleSubstrate {
    switch architecture {
    case "arm64": return .appleVirtualization
    case "x86_64": return .bestEffortIntel
    default: return .unsupported
    }
}

/// Throwing form used by the Rust machine module when it needs a hard guarantee.
public func requireCapsuleSupport(architecture: String = currentArchitecture()) throws {
    let substrate = capsuleSubstrate(architecture: architecture)
    if substrate == .unsupported {
        throw MacBridgeError.capsuleUnsupported("no local capsule substrate for \(architecture)")
    }
}

public func currentArchitecture() -> String {
    #if arch(arm64)
    return "arm64"
    #elseif arch(x86_64)
    return "x86_64"
    #else
    return "unknown"
    #endif
}
