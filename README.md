# jf

A lightweight HTPC client for [Jellyfin](https://jellyfin.org), with a Prime Video
style interface and support for the iPhone's Apple TV remote. It plays series, movies
and live TV, with mpv built into the window.

## Features

- Continue watching, Next up, libraries, search
- Series, movies and live TV channels
- Skip intro and a countdown to the next episode
- Keyboard, mouse or the iOS Apple TV remote (Control Center), nothing to install on the phone
- Playback by libmpv, with your own mpv configuration
- English and French

## Install

Download the archive for your system from the
[releases](https://github.com/superlemon57/jf/releases): Linux (x86_64, arm64), Windows
(x64, arm64) or macOS (Apple Silicon, Intel). On Linux, install libmpv first
(`libmpv2` on Debian and Ubuntu, `mpv` on Arch and Fedora).

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
jf                  # start
jf --fullscreen     # start in fullscreen
jf --logout         # forget the saved session
jf --reset-apple-tv # forget paired iPhones
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

## Configuration

`config.json` lives in `~/.config/jf/` (Linux), `~/Library/Application Support/jf/`
(macOS) or `%APPDATA%\jf\` (Windows).

| Key | Default | |
|---|---|---|
| `language` | `"en"` | `"en"` or `"fr"` |
| `apple_tv` | `"jf"` | name shown in the iPhone remote, `""` to turn it off |

## Credits

The Companion Link protocol was reverse-engineered by [pyatv](https://pyatv.dev). The
OPACK, SRP and encryption code comes from [myRemote](https://github.com/oseanstudio/myRemote)
(MIT, `LICENSES/myRemote-MIT.txt`). The iOS 26/27 behavior follows
[atv-core](https://github.com/corvofeng/atv-core),
[windows-apple-remote](https://github.com/aryakvn/windows-apple-remote) and
[atvr4samsung](https://github.com/vb3/atvr4samsung).
