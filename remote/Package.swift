// swift-tools-version: 6.0

import PackageDescription

let package = Package(
    name: "JFRemote",
    platforms: [
        .iOS(.v17),
    ],
    products: [
        // xtool attend exactement une bibliothèque : l'application.
        .library(name: "JFRemote", targets: ["JFRemote"]),
    ],
    targets: [
        .target(
            name: "JFRemote",
            swiftSettings: [.swiftLanguageMode(.v5)]
        ),
    ]
)
