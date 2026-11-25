# jf

Client [Jellyfin](https://jellyfin.org) natif pour le salon : une interface plein écran
pilotée au clavier ou à la télécommande, avec mpv intégré dans la fenêtre.

- Reprise, « À suivre », bibliothèques, séries, musique, recherche
- TV en direct : la liste des chaînes, sans guide
- Lecture dans l'application par libmpv, avec votre configuration mpv
- « Passer l'intro » et compte à rebours vers l'épisode suivant
- Enchaînement des épisodes, lecture aléatoire, marquage « vu »
- Position de lecture synchronisée avec le serveur
- Pilotable par la télécommande Apple TV d'iOS (Centre de contrôle), sans rien installer

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

Lancer un épisode enchaîne sur les suivants ; lancer une chaîne permet
de zapper avec « suivant » et « précédent ».

### Lecture

jf affiche sa propre interface de lecture au mouvement de la souris ou à l'appui
d'une touche : logo et titre (logo du film, de la série ou de la chaîne quand Jellyfin
en a un), recul et avance de 10 secondes, pause, barre de progression cliquable, et un
bouton « Sous-titres et audio ». Un clic met en pause, un double clic bascule le plein
écran. Les sous-titres remontent au-dessus de la barre tant qu'elle est affichée.

| Touche | Action |
|---|---|
| `Entrée` | valider « Passer l'intro » ou « Épisode suivant », sinon pause |
| `Échap`, `Retour arrière`, `q` | écarter le compte à rebours, sinon arrêter (la position est conservée) |
| `c` | panneau « Sous-titres et audio » : flèches pour choisir, `Entrée` pour valider, `Échap` pour fermer |
| autres touches | transmises à mpv : `Espace`, flèches, `j` pour les sous-titres, `#` pour l'audio… |

**Passer l'intro** apparaît pendant le générique de début (et « Passer le récap »
pendant un récapitulatif), à partir des segments connus du serveur : Jellyfin 10.10
ou plus récent, avec un plugin qui les détecte comme
[Intro Skipper](https://github.com/intro-skipper/intro-skipper).

**Épisode suivant** : au début du générique de fin, ou dans les dernières secondes
s'il n'est pas connu, une carte annonce la suite et la lance après 10 secondes de
lecture. « Voir le générique » laisse finir l'épisode. Un épisode lancé seul (depuis
« Reprendre », « À suivre » ou une recherche) enchaîne aussi sur la suite de la série.

Pour personnaliser le lecteur, déposez vos fichiers dans `~/.config/jf/mpv/` :
`mpv.conf`, `input.conf`, `scripts/`, `script-opts/`, `shaders/`… comme pour mpv.

## Configuration

`~/.config/jf/config.json` contient la session et un réglage :

| Clé | Rôle |
|---|---|
| `apple_tv` | nom affiché dans la télécommande Apple TV d'iOS (`jf` par défaut, `""` pour la désactiver) |

Les images sont mises en cache dans `~/.cache/jf/`.

## Télécommande Apple TV d'iOS

jf se présente sur le réseau local comme une Apple TV : la télécommande du Centre de
contrôle de l'iPhone (et l'app « Remote ») le trouve sous le nom `jf`.

1. Sur l'iPhone, ouvrez la télécommande Apple TV et choisissez `jf`.
2. jf affiche un code à quatre chiffres : saisissez-le sur l'iPhone.

| Sur l'iPhone | Dans jf |
|---|---|
| glisser, flèches | se déplacer |
| toucher, bouton central | valider |
| appui long (pavé ou bouton central) | menu d'options (lecture aléatoire, marquer comme vu…) |
| retour | page précédente |
| TV | accueil (arrête la lecture) |
| lecture/pause, ±10 s | commandes de lecture |
| boutons de volume | volume du système |
| chaîne + / − | élément suivant ou précédent (zapper en TV en direct) |
| sous-titres | activer ou couper les sous-titres |

Pendant la lecture, l'interface se pilote aux flèches :

- interface masquée : gauche/droite reculent ou avancent de 10 s, le bouton central met
  en pause (ou valide « Passer l'intro », « Épisode suivant »), haut/bas l'affichent ;
- interface affichée : les flèches déplacent la sélection entre le bouton
  « Sous-titres et audio » en haut à droite, les commandes centrales, la proposition
  affichée et la barre de progression (gauche/droite y reculent ou avancent) ; le bouton
  central active l'élément sélectionné ;
- l'appui long ouvre directement le panneau « Sous-titres et audio » ;
- retour ferme le panneau, puis masque l'interface, puis écarte le compte à rebours,
  puis arrête la lecture (la position est conservée).

Seuls les iPhone appairés avec le code affiché à l'écran peuvent piloter jf ; leurs clés
sont dans `~/.config/jf/apple-tv/`. `jf --reset-apple-tv` (jf fermé) oublie tous les
iPhone et donne à jf une nouvelle identité : l'iPhone le voit comme un nouvel appareil,
ce qui règle aussi les connexions qui restent bloquées.

En cas de problème, `JF_LOG=1 jf 2> jf-apple-tv.log` enregistre tout l'échange.
Le pare-feu doit laisser passer Bonjour (UDP 5353) et le port TCP 49153.

Le protocole (Companion Link) a été reconstitué par [pyatv](https://pyatv.dev) ; le codec
OPACK, l'appairage SRP et le chiffrement viennent de
[myRemote](https://github.com/oseanstudio/myRemote), sous licence MIT
(`LICENSES/myRemote-MIT.txt`).

## Développement

```sh
nix develop
cargo run
JF_LOG=1 cargo run   # avec le journal du protocole de la télécommande Apple TV
```

| Fichier | Rôle |
|---|---|
| `src/app.rs` | état de l'application et boucle principale |
| `src/jellyfin.rs` | client de l'API Jellyfin |
| `src/mpv.rs` | liaison libmpv et rendu OpenGL |
| `src/player.rs` | file de lecture et suivi de position |
| `src/images.rs` | chargement et cache des images |
| `src/remote.rs` | état et commandes partagés avec la télécommande, volume du système |
| `src/atv/` | jf vu comme une Apple TV par la télécommande d'iOS |
| `src/ui/` | écrans : navigation, lecture et son interface (`osd.rs`), connexion, menu |
