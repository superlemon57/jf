# jf

Client [Jellyfin](https://jellyfin.org) natif pour le salon : une interface plein écran
pilotée au clavier ou à la télécommande, avec mpv intégré dans la fenêtre.

- Reprise, « À suivre », bibliothèques, séries, musique, recherche
- TV en direct : la liste des chaînes, sans guide
- Lecture dans l'application par libmpv, avec votre configuration mpv
- Enchaînement des épisodes, lecture aléatoire, marquage « vu »
- Position de lecture synchronisée avec le serveur
- `remote/` : **jf Remote**, la télécommande pour iPhone

Écrit en Rust avec [egui](https://github.com/emilk/egui). Linux, Wayland ou X11.
Serveur Jellyfin 10.9 ou plus récent.

## Installation

Avec Nix :

```sh
nix run github:superlemon57/jf            # essayer
nix profile install github:superlemon57/jf
```

Ou comme entrée d'un flake NixOS / home-manager :

```nix
inputs.jf = {
  url = "github:superlemon57/jf";
  inputs.nixpkgs.follows = "nixpkgs";
};

home.packages = [ inputs.jf.packages.${pkgs.stdenv.hostPlatform.system}.default ];
```

Le paquet installe un lanceur `jf.desktop`. jf n'a pas d'icône propre : il utilise
l'icône `jellyfin` du thème d'icônes du système (Papirus, Adwaita…).

Sans Nix, il faut Rust, `pkg-config` et les en-têtes de libmpv, puis :

```sh
cargo build --release
```

À l'exécution, jf utilise `fc-match` (fontconfig) pour trouver la police du système
et `wpctl` (PipeWire) pour le volume demandé par la télécommande.

## Utilisation

```sh
jf               # fenêtre
jf --fullscreen  # plein écran
jf --logout      # oublier la session
```

Au premier lancement, jf demande l'adresse du serveur, puis un mot de passe ou un code
Quick Connect.

### Navigation

| Touche | Action |
|---|---|
| Flèches | se déplacer |
| `Entrée` | ouvrir ou lire (reprise automatique) |
| `M` ou `O` | options : lire depuis le début, lecture aléatoire, marquer comme vu |
| `B` | lire depuis le début |
| `W` | marquer comme vu / non vu |
| `Retour arrière`, `Échap` | page précédente |
| `Début` | accueil |
| `/` | rechercher |
| `F`, `F11` | plein écran |

Lancer un épisode depuis une saison enchaîne sur les suivants ; lancer une chaîne permet
de zapper avec « suivant » et « précédent ».

### Lecture

Pendant la lecture, le clavier et la souris sont transmis à mpv : ses raccourcis
habituels s'appliquent (`Espace`, flèches, `j` pour les sous-titres, `#` pour l'audio…)
et son interface apparaît au mouvement de la souris. `Échap`, `Retour arrière` et `q`
arrêtent la lecture en conservant la position.

Pour personnaliser le lecteur, déposez vos fichiers dans `~/.config/jf/mpv/` :
`mpv.conf`, `input.conf`, `scripts/`, `script-opts/`, `shaders/`… comme pour mpv.

## Configuration

`~/.config/jf/config.json` contient la session et un réglage :

| Clé | Rôle |
|---|---|
| `remote` | adresse d'écoute de l'API télécommande (`0.0.0.0:8097` par défaut, `""` pour la désactiver) |

Les images sont mises en cache dans `~/.cache/jf/`.

## Télécommande

jf expose une petite API HTTP sur le réseau local, utilisée par jf Remote.
**Elle n'est pas authentifiée** : toute machine du réseau peut piloter la lecture.
Le jeton Jellyfin n'est jamais exposé. Pensez à ouvrir le port 8097 du pare-feu.

| Requête | Effet |
|---|---|
| `GET /api/state` | état de la lecture, pistes audio et sous-titres |
| `GET /api/artwork` | affiche de l'élément en cours |
| `POST /api/key {"key": "up"}` | navigation : `up`, `down`, `left`, `right`, `ok`, `back`, `home`, `menu` ; lecture : `playpause`, `play`, `pause`, `stop`, `next`, `previous` |
| `POST /api/seek {"position": 120}` | saut absolu en secondes, ou `{"offset": -10}` |
| `POST /api/track {"kind": "subtitle", "id": 2}` | piste `audio` ou `subtitle` ; `id` nul : aucune |
| `POST /api/volume {"delta": 0.05}` | volume du système |

### jf Remote (iOS)

Application SwiftUI dans `remote/`, construite avec [xtool](https://github.com/xtool-org/xtool) :

```sh
cd remote
xtool dev build -c release --ipa   # produit xtool/JFRemote.ipa
```

- Croix directionnelle, retour, accueil, options
- Pendant la lecture : pause, sauts de 10 secondes, précédent/suivant, curseur de position,
  choix des sous-titres et de la piste audio, arrêt (la position est conservée)
- Les boutons de volume du téléphone règlent le volume de l'ordinateur tant que
  l'application est au premier plan
- Lecture en cours et commandes sur l'écran de verrouillage

Pour apparaître sur l'écran de verrouillage, l'application joue un silence pendant que jf
lit quelque chose : la musique du téléphone est alors mise en pause.

## Développement

```sh
nix develop
cargo run
```

| Fichier | Rôle |
|---|---|
| `src/app.rs` | état de l'application et boucle principale |
| `src/jellyfin.rs` | client de l'API Jellyfin |
| `src/mpv.rs` | liaison libmpv et rendu OpenGL |
| `src/player.rs` | file de lecture et suivi de position |
| `src/images.rs` | chargement et cache des images |
| `src/remote.rs` | API télécommande |
| `src/ui/` | écrans : navigation, lecture, connexion, menu |
