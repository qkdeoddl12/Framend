import SwiftUI

@main
struct RecoveryCardApp: App {
    @State private var model = RecoveryModel()
    var body: some Scene {
        Window("Recovery Card", id: "main") {
            ContentView(model: model)
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
