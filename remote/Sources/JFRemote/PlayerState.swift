import Foundation

struct Track: Codable, Identifiable, Equatable {
    let id: Int
    let label: String
    let selected: Bool
}

/// État de la lecture, tel que renvoyé par `GET /api/state`.
struct PlayerState: Codable, Equatable {
    var playing = false
    var paused = false
    var live = false
    var title = ""
    var position = 0.0
    var duration = 0.0
    var hasNext = false
    var hasPrevious = false
    var audio: [Track] = []
    var subtitles: [Track] = []

    enum CodingKeys: String, CodingKey {
        case playing, paused, live, title, position, duration, audio, subtitles
        case hasNext = "has_next"
        case hasPrevious = "has_previous"
    }
}

enum TrackKind: String {
    case audio
    case subtitle
}

func clock(_ seconds: Double) -> String {
    let total = Int(max(seconds, 0))
    let (hours, minutes, rest) = (total / 3600, total % 3600 / 60, total % 60)
    return hours > 0
        ? String(format: "%d:%02d:%02d", hours, minutes, rest)
        : String(format: "%d:%02d", minutes, rest)
}
