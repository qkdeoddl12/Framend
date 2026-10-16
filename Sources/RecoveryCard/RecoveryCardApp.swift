import SwiftUI

@main
struct RecoveryCardApp: App {
    @State private var model = RecoveryModel()
    @State private var video = VideoRecoveryModel()
    var body: some Scene {
        Window("Framend", id: "main") {
            TabView {
                ContentView(model: model).tabItem { Label("사진 복구", systemImage: "photo") }
                VideoRecoveryView(model: video).tabItem { Label("영상 복구", systemImage: "film") }
            }
                .frame(minWidth: 1000, minHeight: 640)
                .tint(Color(red: 0.2, green: 0.55, blue: 0.43))
                .onAppear {
                    NSApplication.shared.setActivationPolicy(.regular)
                    NSApplication.shared.activate(ignoringOtherApps: true)
                }
        }
        .defaultSize(width: 1200, height: 760)
    }
}
