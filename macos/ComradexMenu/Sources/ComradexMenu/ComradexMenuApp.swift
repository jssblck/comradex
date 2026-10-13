import AppKit
import ComradexWidgetCore

enum StatusIcon {
    static let image: NSImage = {
        let url = Bundle.main.url(forResource: "comradex-logo", withExtension: "svg")
            ?? Bundle.module.url(forResource: "comradex-logo", withExtension: "svg")!
        let image = NSImage(contentsOf: url)!
        image.size = NSSize(width: 19, height: 13)
        image.accessibilityDescription = "Comradex"
        image.isTemplate = true
        return image
    }()
}

@main
enum ComradexMenuApplication {
    @MainActor
    static func main() {
        let application = NSApplication.shared
        let delegate = AppDelegate()
        application.delegate = delegate
        application.setActivationPolicy(.accessory)
        withExtendedLifetime(delegate) {
            application.run()
        }
    }
}

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    private let store = ComradexStore()
    private var menuController: MenuBarController?

    func applicationDidFinishLaunching(_ notification: Notification) {
        let controller = MenuBarController(store: store)
        menuController = controller
        controller.start()
    }

    // Clicking the desktop widget opens its URL, which shows the status menu.
    func application(_ application: NSApplication, open urls: [URL]) {
        guard let expected = usageMenuURL(), urls.contains(expected) else { return }
        // Let the open-URL event finish before the menu starts tracking.
        DispatchQueue.main.async { [weak self] in self?.menuController?.showMenu() }
    }
}
