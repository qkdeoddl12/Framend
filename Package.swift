// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "RecoveryCard",
    platforms: [.macOS(.v14)],
    products: [.executable(name: "RecoveryCard", targets: ["RecoveryCard"])],
    targets: [
        .target(name: "RecoveryCore"),
        .executableTarget(name: "RecoveryCard", dependencies: ["RecoveryCore"]),
        .executableTarget(name: "RecoveryChecks", dependencies: ["RecoveryCore"], path: "Tests/RecoveryCoreTests")
    ]
)
