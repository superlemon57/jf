import AVFoundation
import MediaPlayer
import UIKit

/// Affiche la lecture de jf sur l'écran de verrouillage et relaie ses boutons.
///
/// iOS ne montre ces commandes que pour l'application qui produit du son :
/// tant que jf lit quelque chose, on joue donc un silence en boucle. Cela garde
/// aussi l'application active en arrière-plan pour suivre la position.
@MainActor
final class NowPlaying {
    weak var remote: Remote?

    private let silence: AVAudioPlayer? = {
        let player = try? AVAudioPlayer(data: silentWAV(seconds: 1))
        player?.numberOfLoops = -1
        return player
    }()
    private var exclusive: Bool?
    private var artworkTitle: String?
    private var artwork: MPMediaItemArtwork?

    init() {
        let center = MPRemoteCommandCenter.shared()
        let press: (String) -> (MPRemoteCommandEvent) -> MPRemoteCommandHandlerStatus = { key in
            { [weak self] _ in
                self?.remote?.press(key)
                return .success
            }
        }
        center.playCommand.addTarget(handler: press("play"))
        center.pauseCommand.addTarget(handler: press("pause"))
        center.togglePlayPauseCommand.addTarget(handler: press("playpause"))
        center.nextTrackCommand.addTarget(handler: press("next"))
        center.previousTrackCommand.addTarget(handler: press("previous"))
        center.skipForwardCommand.preferredIntervals = [10]
        center.skipForwardCommand.addTarget { [weak self] _ in
            self?.remote?.seek(by: 10)
            return .success
        }
        center.skipBackwardCommand.preferredIntervals = [10]
        center.skipBackwardCommand.addTarget { [weak self] _ in
            self?.remote?.seek(by: -10)
            return .success
        }
        center.changePlaybackPositionCommand.addTarget { [weak self] event in
            guard let event = event as? MPChangePlaybackPositionCommandEvent else { return .commandFailed }
            self?.remote?.seek(to: event.positionTime)
            return .success
        }
        configureSession(exclusive: false)
    }

    /// Session audio partagée au repos (la musique du téléphone continue),
    /// exclusive pendant la lecture (condition pour l'écran de verrouillage).
    private func configureSession(exclusive wanted: Bool) {
        guard exclusive != wanted else { return }
        exclusive = wanted
        let session = AVAudioSession.sharedInstance()
        try? session.setCategory(.playback, options: wanted ? [] : [.mixWithOthers])
        try? session.setActive(true)
    }

    func update(_ state: PlayerState) {
        let center = MPRemoteCommandCenter.shared()
        let info = MPNowPlayingInfoCenter.default()
        guard state.playing else {
            silence?.stop()
            info.nowPlayingInfo = nil
            artworkTitle = nil
            artwork = nil
            configureSession(exclusive: false)
            return
        }

        configureSession(exclusive: true)
        if state.paused {
            silence?.pause()
        } else if silence?.isPlaying == false {
            silence?.play()
        }

        // Boutons précédent/suivant s'il y a une file, sinon sauts de 10 secondes.
        let queue = state.hasNext || state.hasPrevious
        center.nextTrackCommand.isEnabled = state.hasNext
        center.previousTrackCommand.isEnabled = state.hasPrevious
        center.skipForwardCommand.isEnabled = !queue && !state.live
        center.skipBackwardCommand.isEnabled = !queue && !state.live
        center.changePlaybackPositionCommand.isEnabled = !state.live

        var values: [String: Any] = [
            MPMediaItemPropertyTitle: state.title,
            MPNowPlayingInfoPropertyPlaybackRate: state.paused ? 0.0 : 1.0,
            MPNowPlayingInfoPropertyIsLiveStream: state.live,
        ]
        if !state.live {
            values[MPNowPlayingInfoPropertyElapsedPlaybackTime] = state.position
            values[MPMediaItemPropertyPlaybackDuration] = state.duration
        }
        if let artwork {
            values[MPMediaItemPropertyArtwork] = artwork
        }
        info.nowPlayingInfo = values

        if artworkTitle != state.title {
            artworkTitle = state.title
            artwork = nil
            loadArtwork(for: state.title)
        }
    }

    private func loadArtwork(for title: String) {
        guard let url = remote?.artworkURL else { return }
        Task {
            guard let (data, _) = try? await URLSession.shared.data(from: url),
                  let image = UIImage(data: data),
                  artworkTitle == title
            else { return }
            artwork = MPMediaItemArtwork(boundsSize: image.size) { _ in image }
            MPNowPlayingInfoCenter.default().nowPlayingInfo?[MPMediaItemPropertyArtwork] = artwork
        }
    }
}

/// Fichier WAV mono 16 bits ne contenant que du silence.
private func silentWAV(seconds: Int) -> Data {
    let rate: UInt32 = 8000
    let size = rate * 2 * UInt32(seconds)
    var data = Data()
    func append<T: FixedWidthInteger>(_ value: T) {
        withUnsafeBytes(of: value.littleEndian) { data.append(contentsOf: $0) }
    }
    data.append(contentsOf: Array("RIFF".utf8))
    append(36 + size)
    data.append(contentsOf: Array("WAVEfmt ".utf8))
    append(UInt32(16))      // taille du bloc de format
    append(UInt16(1))       // PCM
    append(UInt16(1))       // mono
    append(rate)
    append(rate * 2)        // octets par seconde
    append(UInt16(2))       // octets par échantillon
    append(UInt16(16))      // bits par échantillon
    data.append(contentsOf: Array("data".utf8))
    append(size)
    data.append(Data(count: Int(size)))
    return data
}
