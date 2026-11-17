import Foundation
import SwiftUI

/// Connexion à jf : interroge son état chaque seconde et lui envoie les commandes.
@MainActor
final class Remote: ObservableObject {
    @Published private(set) var state = PlayerState()
    @Published private(set) var connected = false
    /// Volume du système distant (0 à 1) après un appui, le temps de l'afficher.
    @Published private(set) var volume: Double?
    @Published var address: String {
        didSet { UserDefaults.standard.set(address, forKey: "address") }
    }

    private let nowPlaying = NowPlaying()
    private let session: URLSession
    private var polling: Task<Void, Never>?
    private var volumeHide: Task<Void, Never>?

    init() {
        address = UserDefaults.standard.string(forKey: "address") ?? ""
        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForRequest = 3
        session = URLSession(configuration: configuration)
        nowPlaying.remote = self
    }

    /// Adresse saisie, complétée du schéma et du port par défaut de jf.
    private var base: URL? {
        var text = address.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return nil }
        if !text.contains("://") { text = "http://" + text }
        guard var parts = URLComponents(string: text) else { return nil }
        if parts.port == nil { parts.port = 8097 }
        parts.path = ""
        return parts.url
    }

    var artworkURL: URL? { base?.appending(path: "api/artwork") }

    func start() {
        guard polling == nil else { return }
        polling = Task {
            while !Task.isCancelled {
                await refresh()
                try? await Task.sleep(for: .seconds(1))
            }
        }
    }

    func refresh() async {
        var fresh: PlayerState?
        if let url = base?.appending(path: "api/state"),
           let (data, _) = try? await session.data(from: url) {
            fresh = try? JSONDecoder().decode(PlayerState.self, from: data)
        }
        connected = fresh != nil
        let next = fresh ?? PlayerState()
        if next != state { state = next }
        nowPlaying.update(state)
    }

    @discardableResult
    private func post(_ path: String, _ body: [String: Any]) async -> [String: Any]? {
        guard let url = base?.appending(path: "api/" + path) else { return nil }
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try? JSONSerialization.data(withJSONObject: body)
        guard let (data, _) = try? await session.data(for: request) else { return nil }
        return try? JSONSerialization.jsonObject(with: data) as? [String: Any]
    }

    private func command(_ path: String, _ body: [String: Any]) {
        Task {
            await post(path, body)
            await refresh()
        }
    }

    func press(_ key: String) { command("key", ["key": key]) }
    func seek(to position: Double) { command("seek", ["position": position]) }
    func seek(by offset: Double) { command("seek", ["offset": offset]) }

    func select(_ kind: TrackKind, _ id: Int?) {
        command("track", ["kind": kind.rawValue, "id": id ?? NSNull()])
    }

    /// Monte ou baisse le volume du système distant d'un cran.
    func stepVolume(_ direction: Int) {
        Task {
            let reply = await post("volume", ["delta": 0.05 * Double(direction)])
            volume = reply?["volume"] as? Double
            volumeHide?.cancel()
            volumeHide = Task {
                try? await Task.sleep(for: .seconds(1.5))
                if !Task.isCancelled { volume = nil }
            }
        }
    }
}
