import SwiftUI

/// Grand bouton de télécommande : icône, libellé, retour haptique.
struct RemoteButton: View {
    let label: String
    let icon: String
    var prominent = false
    var showsLabel = true
    var repeats = false
    let action: () -> Void

    var body: some View {
        Button {
            UIImpactFeedbackGenerator(style: .medium).impactOccurred()
            action()
        } label: {
            VStack(spacing: 6) {
                Image(systemName: icon)
                    .font(prominent ? .largeTitle : .title)
                    .fontWeight(.semibold)
                if showsLabel {
                    Text(label)
                        .font(.footnote)
                        .lineLimit(1)
                        .minimumScaleFactor(0.7)
                }
            }
            .frame(maxWidth: .infinity, minHeight: prominent ? 96 : 76)
            .contentShape(Rectangle())
        }
        .buttonStyle(RemoteButtonStyle(prominent: prominent))
        .buttonRepeatBehavior(repeats ? .enabled : .disabled)
        .accessibilityLabel(label)
    }
}

struct RemoteButtonStyle: ButtonStyle {
    let prominent: Bool

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .foregroundStyle(prominent ? Color.black : Color.white)
            .background(
                RoundedRectangle(cornerRadius: 18, style: .continuous)
                    .fill(prominent ? Color.white : Color(white: 0.16))
            )
            .opacity(configuration.isPressed ? 0.55 : 1)
    }
}

/// Croix directionnelle. Maintenir une flèche répète le déplacement.
struct DirectionPad: View {
    let press: (String) -> Void

    var body: some View {
        Grid(horizontalSpacing: 10, verticalSpacing: 10) {
            GridRow {
                Color.clear.gridCellUnsizedAxes([.horizontal, .vertical])
                arrow("Haut", "chevron.up", "up")
                Color.clear.gridCellUnsizedAxes([.horizontal, .vertical])
            }
            GridRow {
                arrow("Gauche", "chevron.left", "left")
                RemoteButton(label: "Valider", icon: "circle.fill", prominent: true, showsLabel: false) { press("ok") }
                arrow("Droite", "chevron.right", "right")
            }
            GridRow {
                Color.clear.gridCellUnsizedAxes([.horizontal, .vertical])
                arrow("Bas", "chevron.down", "down")
                Color.clear.gridCellUnsizedAxes([.horizontal, .vertical])
            }
        }
    }

    private func arrow(_ label: String, _ icon: String, _ key: String) -> some View {
        RemoteButton(label: label, icon: icon, prominent: true, showsLabel: false, repeats: true) { press(key) }
    }
}

/// Choix d'une piste audio ou de sous-titres.
struct TrackPicker: View {
    let title: String
    let tracks: [Track]
    /// Propose « Aucun » en tête de liste (sous-titres).
    let allowsNone: Bool
    let select: (Int?) -> Void
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            List {
                if allowsNone {
                    row("Aucun", selected: !tracks.contains(where: \.selected), id: nil)
                }
                ForEach(tracks) { track in
                    row(track.label, selected: track.selected, id: track.id)
                }
            }
            .navigationTitle(title)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Fermer") { dismiss() }
                }
            }
        }
        .presentationDetents([.medium, .large])
    }

    private func row(_ label: String, selected: Bool, id: Int?) -> some View {
        Button {
            select(id)
            dismiss()
        } label: {
            HStack {
                Text(label).font(.title3)
                Spacer()
                if selected {
                    Image(systemName: "checkmark").fontWeight(.semibold)
                }
            }
            .frame(minHeight: 44)
        }
        .foregroundStyle(.primary)
        .accessibilityAddTraits(selected ? .isSelected : [])
    }
}

/// Saisie de l'adresse de l'ordinateur où tourne jf.
struct AddressForm: View {
    @Binding var address: String
    @State private var draft = ""
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("192.168.1.20", text: $draft)
                        .keyboardType(.URL)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .font(.title3)
                        .accessibilityLabel("Adresse de jf")
                } header: {
                    Text("Adresse de jf")
                } footer: {
                    Text("Adresse de l'ordinateur où tourne jf, sur le même réseau. Le port 8097 est utilisé par défaut.")
                }
            }
            .navigationTitle("Connexion")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Enregistrer") {
                        address = draft
                        dismiss()
                    }
                }
            }
            .onAppear { draft = address }
        }
    }
}
