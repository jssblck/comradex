import Foundation

/// Reads and writes the usage snapshot shared between the menu app and its widget.
public struct UsageSnapshotStore: Sendable {
    public static let fileName = "usage-snapshot.json"

    /// The shared file, or nil when the build has no app group (ad-hoc signing).
    public let fileURL: URL?

    public init(directory: URL?) {
        fileURL = directory?.appendingPathComponent(Self.fileName)
    }

    /// Uses the app group named by the bundle's `ComradexAppGroup` key. Packaging sets it
    /// only when the build is signed with a team that owns the group.
    public init(bundle: Bundle = .main) {
        let group = bundle.object(forInfoDictionaryKey: "ComradexAppGroup") as? String
        self.init(directory: group.flatMap {
            FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: $0)
        })
    }

    public func load() -> UsageSnapshot? {
        guard let fileURL, let data = try? Data(contentsOf: fileURL) else { return nil }
        return try? Self.decoder.decode(UsageSnapshot.self, from: data)
    }

    public func save(_ snapshot: UsageSnapshot) throws {
        guard let fileURL else { return }
        try FileManager.default.createDirectory(
            at: fileURL.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        try Self.encoder.encode(snapshot).write(to: fileURL, options: .atomic)
    }

    private static let encoder: JSONEncoder = {
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .secondsSince1970
        encoder.outputFormatting = [.sortedKeys]
        return encoder
    }()

    private static let decoder: JSONDecoder = {
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .secondsSince1970
        return decoder
    }()
}

/// The URL that opens the menu app's status menu, from the bundle's `ComradexURLScheme` key.
public func usageMenuURL(bundle: Bundle = .main) -> URL? {
    guard let scheme = bundle.object(forInfoDictionaryKey: "ComradexURLScheme") as? String else { return nil }
    return URL(string: "\(scheme)://usage")
}

/// WidgetKit kind of the usage widget, shared so the menu app can reload its timelines.
public let usageWidgetKind = "com.nicosuave.comradex.usage"
