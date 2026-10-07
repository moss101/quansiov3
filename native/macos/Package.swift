// swift-tools-version: 5.9
import PackageDescription

// Quansio macOS native bridge (DOSSIER.md §17, §3 "Native platform code").
// Canonical owner: native/macos. Called by the Rust machine module through a narrow
// typed interface; contains no product orchestration and no runtime authority.
//
// `quansio-recorder` (ADP-003) is the person's recorder: a short-lived process the desktop starts for the
// length of one recording. It is not a service and installs nothing.
let package = Package(
    name: "QuansioMacBridge",
    platforms: [.macOS(.v14)],
    products: [
        .library(name: "QuansioMacBridge", targets: ["QuansioMacBridge"]),
        .executable(name: "quansio-recorder", targets: ["quansio-recorder"]),
    ],
    targets: [
        .target(name: "QuansioMacBridge"),
        .executableTarget(name: "quansio-recorder", dependencies: ["QuansioMacBridge"]),
        .testTarget(name: "QuansioMacBridgeTests", dependencies: ["QuansioMacBridge"]),
    ]
)
