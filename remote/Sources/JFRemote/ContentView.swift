import SwiftUI

struct ContentView: View {
    @StateObject private var remote = Remote()
    @StateObject private var volumeButtons = VolumeButtons()
    @Environment(\.scenePhase) private var phase
    @State private var sheet: Sheet?

    private enum Sheet: String, Identifiable {
        case address, audio, subtitles
        var id: String { rawValue }
    }

    var body: some View {
        VStack(spacing: 18) {
            header
            if remote.state.playing {
                PlaybackControls(remote: remote) { sheet = $0 == .audio ? .audio : .subtitles }
            } else {
                browseControls
            }
        }
        .padding(20)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Color.black.ignoresSafeArea())
        .background(VolumeButtonsView(buttons: volumeButtons).frame(width: 1, height: 1))
        .sheet(item: $sheet) { sheet in
            switch sheet {
            case .address:
                AddressForm(address: $remote.address)
            case .audio:
                TrackPicker(title: "Audio", tracks: remote.state.audio, allowsNone: false) {
                    remote.select(.audio, $0)
                }
            case .subtitles:
                TrackPicker(title: "Sous-titres", tracks: remote.state.subtitles, allowsNone: true) {
                    remote.select(.subtitle, $0)
                }
            }
        }
        .onAppear {
            remote.start()
            volumeButtons.onPress = { remote.stepVolume($0) }
            if remote.address.isEmpty { sheet = .address }
        }
        .onChange(of: phase, initial: true) { _, phase in
            // Les boutons de volume ne sont détournés qu'au premier plan.
            if phase == .active { volumeButtons.start() } else { volumeButtons.stop() }
        }
    }

    private var header: some View {
        HStack {
            if let volume = remote.volume {
                Label("Volume \(Int((volume * 100).rounded())) %", systemImage: "speaker.wave.2.fill")
                    .font(.headline)
            } else if !remote.connected {
                Label("jf est injoignable", systemImage: "wifi.slash")
                    .font(.headline)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            Button { sheet = .address } label: {
                Image(systemName: "gearshape.fill")
                    .font(.title2)
                    .frame(width: 48, height: 48)
            }
            .foregroundStyle(.secondary)
            .accessibilityLabel("Adresse de jf")
        }
        .frame(minHeight: 48)
    }

    private var browseControls: some View {
        VStack(spacing: 18) {
            Spacer(minLength: 0)
            DirectionPad { remote.press($0) }
            Spacer(minLength: 0)
            HStack(spacing: 10) {
                RemoteButton(label: "Retour", icon: "arrow.uturn.backward") { remote.press("back") }
                RemoteButton(label: "Accueil", icon: "house.fill") { remote.press("home") }
                RemoteButton(label: "Options", icon: "ellipsis") { remote.press("menu") }
            }
        }
    }
}

/// Commandes affichées pendant une lecture.
struct PlaybackControls: View {
    @ObservedObject var remote: Remote
    let pick: (TrackKind) -> Void

    /// Position choisie au curseur, tant que le doigt ne l'a pas relâché.
    @State private var scrub: Double?

    private var state: PlayerState { remote.state }

    var body: some View {
        VStack(spacing: 18) {
            Spacer(minLength: 0)
            Text(state.title)
                .font(.title2.weight(.semibold))
                .multilineTextAlignment(.center)
                .lineLimit(4)
                .minimumScaleFactor(0.8)
                .accessibilityAddTraits(.isHeader)
            if !state.live && state.duration > 0 {
                progress
            }
            Spacer(minLength: 0)
            transport
            HStack(spacing: 10) {
                if !state.subtitles.isEmpty {
                    RemoteButton(label: "Sous-titres", icon: "captions.bubble.fill") { pick(.subtitle) }
                }
                if state.audio.count > 1 {
                    RemoteButton(label: "Audio", icon: "waveform") { pick(.audio) }
                }
                RemoteButton(label: "Arrêter", icon: "stop.fill") { remote.press("stop") }
            }
        }
    }

    private var progress: some View {
        VStack(spacing: 4) {
            Slider(
                value: Binding(get: { scrub ?? state.position }, set: { scrub = $0 }),
                in: 0...state.duration
            ) { editing in
                if !editing, let target = scrub {
                    remote.seek(to: target)
                    scrub = nil
                }
            }
            .tint(.white)
            .accessibilityLabel("Position")
            .accessibilityValue("\(clock(scrub ?? state.position)) sur \(clock(state.duration))")
            HStack {
                Text(clock(scrub ?? state.position))
                Spacer()
                Text(clock(state.duration))
            }
            .font(.subheadline.monospacedDigit())
            .foregroundStyle(.secondary)
            .accessibilityHidden(true)
        }
    }

    private var transport: some View {
        VStack(spacing: 10) {
            RemoteButton(
                label: state.paused ? "Lecture" : "Pause",
                icon: state.paused ? "play.fill" : "pause.fill",
                prominent: true
            ) { remote.press("playpause") }
            HStack(spacing: 10) {
                if state.hasPrevious {
                    RemoteButton(label: "Précédent", icon: "backward.end.fill") { remote.press("previous") }
                }
                if !state.live {
                    RemoteButton(label: "Reculer", icon: "gobackward.10", repeats: true) { remote.seek(by: -10) }
                    RemoteButton(label: "Avancer", icon: "goforward.10", repeats: true) { remote.seek(by: 10) }
                }
                if state.hasNext {
                    RemoteButton(label: "Suivant", icon: "forward.end.fill") { remote.press("next") }
                }
            }
        }
    }
}
