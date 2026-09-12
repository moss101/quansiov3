// swift-tools-version: 5.9
import PackageDescription

// Quansio macOS native bridge (DOSSIER.md §17, §3 "Native platform code").
// Canonical owner: native/macos. Called by the Rust machine module through a narrow
// typed interface; contains no product orchestration and no runtime authority.
let package = Package(
    name: "QuansioMacBridge",
    platforms: [.macOS(.v14)],
    products: [.library(name: "QuansioMacBridge", targets: ["QuansioMacBridge"])],
    targets: [
        .target(name: "QuansioMacBridge"),
        .testTarget(name: "QuansioMacBridgeTests", dependencies: ["QuansioMacBridge"]),
    ]
)
