# jf

[![Rust](https://img.shields.io/badge/Rust-black?logo=rust&logoColor=white)](https://www.rust-lang.org) [![NixOS](https://img.shields.io/badge/NixOS-5277C3?logo=nixos&logoColor=white)](https://nixos.org) [![Linux](https://img.shields.io/badge/Linux-FCC624?logo=linux&logoColor=black)](#install) [![Windows](https://img.shields.io/badge/Windows-0078D4?logo=data:image/svg%2Bxml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHZpZXdCb3g9IjAgMCA4OCA4OCI+PHBhdGggZmlsbD0iI2ZmZiIgZD0iTTAgMGg0MnY0Mkgwek00NiAwaDQydjQySDQ2ek0wIDQ2aDQydjQySDB6TTQ2IDQ2aDQydjQySDQ2eiIvPjwvc3ZnPg==)](#install) [![macOS](https://img.shields.io/badge/macOS-000000?logo=apple&logoColor=white)](#install) [![Jellyfin](https://img.shields.io/badge/Jellyfin-00A4DC?logo=jellyfin&logoColor=white)](https://jellyfin.org) [![Apple TV](https://img.shields.io/badge/Apple%20TV-black?logo=appletv&logoColor=white)](#apple-tv-remote) [![Android TV](https://img.shields.io/badge/Android%20TV-3DDC84?logo=androidtv&logoColor=white)](#android-tv-remote) [![Claude](https://img.shields.io/badge/Claude-D97757?logo=claude&logoColor=white)](https://claude.ai)

A really fast, lightweight HTPC client for [Jellyfin](https://jellyfin.org), with a Prime Video style interface and support for the TV remotes built into iPhones and Android phones.

## Features

- Continue watching, Next up, libraries, search
- Series, movies and live TV channels
- Skip intro/recap and a countdown to the next episode
- Keyboard, mouse, the iPhone's Apple TV remote or the Android TV remote
- Shows up in the cast menu of the other Jellyfin apps, which can play on it and control it
- Playback by libmpv, with your own mpv configuration

## Install

Download the archive for your system from the [releases](https://github.com/superlemon57/jf/releases). On Linux, install libmpv first (`libmpv2` on Debian and Ubuntu, `mpv` on Arch and Fedora).

With Nix:

```sh
nix run github:superlemon57/jf
```

From source, with Rust and the libmpv headers:

```sh
cargo build --release
```

## Usage

```sh
jf                    # start
jf --fullscreen       # start in fullscreen
jf --logout           # forget the saved session
jf --reset-apple-tv   # forget paired iPhones
jf --reset-android-tv # forget paired Android phones
```

On first launch, jf asks for the server address, then a password or a Quick Connect code.

| Key | Browsing | Playback |
|---|---|---|
| Arrows | move | show the controls, seek |
| `Enter` | open or play | skip intro, next episode, pause |
| `Esc`, `Backspace` | back | stop |
| `M` | options (shuffle, mark as watched…) | |
| `/` | search | |
| `c` | | subtitles and audio |
| `F`, `F11` | fullscreen | fullscreen |

Other keys go to mpv during playback. Your mpv files (`mpv.conf`, `input.conf`,
`scripts/`…) go in the `mpv` folder next to `config.json`.

## Apple TV remote

jf shows up as an Apple TV named `jf` in the iPhone's remote (Control Center). Pick it
and enter the code shown by jf. The firewall must allow Bonjour (UDP 5353) and TCP 49153.

## Android TV remote

jf shows up as a Google TV device named `jf` in the remote of the Google TV app (also
in the quick settings of Android). Pick it and enter the code shown by jf. The firewall
must allow Bonjour (UDP 5353) and TCP 6466–6467.

## Jellyfin remote control

While signed in, jf appears in the cast menu of the other Jellyfin apps (web, Android,
iOS): send a movie or an episode to it, then pause, seek, skip or change the audio and
subtitles from there.

## Configuration

`config.json` lives in `~/.config/jf/` (Linux), `~/Library/Application Support/jf/`
(macOS) or `%APPDATA%\jf\` (Windows).

| Key | Default | |
|---|---|---|
| `language` | `"en"` | `"en"` or `"fr"` *(more languages incoming)* |
| `apple_tv` | `"jf"` | name shown in the iPhone remote, `""` to turn it off |
| `android_tv` | `"jf"` | name shown in the Android remote, `""` to turn it off |

## Credits

The Companion Link protocol was reverse-engineered by [pyatv](https://pyatv.dev). The
OPACK, SRP and encryption code comes from [myRemote](https://github.com/oseanstudio/myRemote)
(MIT, `LICENSES/myRemote-MIT.txt`).
The Android TV Remote protocol follows [androidtvremote2](https://github.com/tronikos/androidtvremote2)
and the notes of [OpenRemote](https://github.com/hirusheth/OpenRemote).
The iOS 26/27 behavior follows [atv-core](https://github.com/corvofeng/atv-core), [windows-apple-remote](https://github.com/aryakvn/windows-apple-remote) and [atvr4samsung](https://github.com/vb3/atvr4samsung).

## License

[MIT](LICENSE).
