import AVFoundation
import MediaPlayer
import SwiftUI

/// Détourne les boutons de volume du téléphone tant que l'application est au premier plan.
///
/// iOS n'offre pas d'API pour cela : on observe le volume du téléphone, on en déduit
/// le bouton pressé, puis on remet le volume à sa valeur de repos pour pouvoir
/// détecter l'appui suivant. Le curseur de `MPVolumeView` est le seul moyen de
/// régler ce volume, et sa présence à l'écran masque l'indicateur du système.
@MainActor
final class VolumeButtons: ObservableObject {
    let view = MPVolumeView(frame: CGRect(x: 0, y: 0, width: 1, height: 1))
    var onPress: ((Int) -> Void)?

    private var observation: NSKeyValueObservation?
    private var original: Float?
    private var resting: Float = 0.5

    private var slider: UISlider? {
        view.subviews.compactMap { $0 as? UISlider }.first
    }

    private func set(_ volume: Float) {
        // Le curseur n'est prêt qu'un instant après l'apparition de la vue.
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.05) { [weak self] in
            self?.slider?.value = volume
        }
    }

    func start() {
        guard observation == nil else { return }
        let session = AVAudioSession.sharedInstance()
        let current = session.outputVolume
        original = current
        // Aux extrémités, un des deux boutons ne changerait plus rien : on s'en écarte.
        resting = min(max(current, 0.2), 0.8)
        if resting != current { set(resting) }
        observation = session.observe(\.outputVolume, options: [.new]) { [weak self] _, change in
            guard let value = change.newValue else { return }
            Task { @MainActor in self?.changed(to: value) }
        }
    }

    private func changed(to value: Float) {
        // Retour à la valeur de repos : c'est notre propre réglage, pas un appui.
        guard abs(value - resting) > 0.001 else { return }
        onPress?(value > resting ? 1 : -1)
        set(resting)
    }

    /// Rend les boutons au téléphone et restaure son volume.
    func stop() {
        guard observation != nil else { return }
        observation = nil
        if let original, original != resting { set(original) }
    }
}

/// Place la vue de volume dans l'interface, invisible mais présente.
struct VolumeButtonsView: UIViewRepresentable {
    let buttons: VolumeButtons

    func makeUIView(context: Context) -> MPVolumeView {
        buttons.view.alpha = 0.0001
        return buttons.view
    }

    func updateUIView(_ view: MPVolumeView, context: Context) {}
}
