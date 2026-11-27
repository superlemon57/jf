# AGENTS.md

Notes for contributors and coding agents working on jf.

## Contributions

Pull requests written with an LLM are welcome, on the same terms as any other: the code
must be clean, working and tested, and it must not bloat the client. jf is meant to stay
small and fast. Prefer a short change in the existing style over a new dependency, a new
abstraction or a new setting, and explain in the pull request what you tested and how.

## Project

- Rust, egui/eframe (glow) for the interface, libmpv for playback through its OpenGL
  render API (`src/mpv.rs`, own bindings).
- `src/ui/`: screens (browse, playback controls, overlays, sign-in).
- `src/jellyfin.rs`: Jellyfin API client; `src/player.rs`: queue and playback.
- `src/atv/`: the device emulated for the iOS Apple TV remote (Companion Link: Bonjour,
  pairing, encrypted session).
- `src/androidtv/`: the device emulated for the Android TV remote (TLS, "Polo" pairing on
  port 6467, remote on port 6466, Protocol Buffers written by hand in `proto.rs`).
- `src/cast.rs`: remote control from other Jellyfin apps (capabilities and the server's
  WebSocket); `src/lan.rs`: local address, Bonjour announcement and protocol logs shared
  by the remotes.
- `src/i18n.rs`: interface strings are written in place in English and French with
  `t("English", "Français")`; English is the default. Only these two languages for now.

## Conventions

- Code, comments, logs and commit messages in English.
- Every user-facing string goes through `t()`.
- `cargo fmt`, `cargo clippy --release` and `cargo test --release` must pass.
- Keep platform-specific code behind `cfg` so Linux, macOS and Windows keep building.

## Building

```sh
nix develop -c cargo build --release   # or plain cargo with the libmpv headers installed
nix build                              # Nix package
```

## Remotes

The phone protocols are reverse-engineered and change between releases. Every exchange
is logged in the cache folder (`~/.cache/jf/` on Linux): `apple-tv.log`,
`android-tv.log` and `jellyfin-remote.log`. Read them before changing a protocol, and
check changes on a real phone. Fake phones (pyatv, androidtvremote2) are useful for
regression tests but do not behave exactly like iOS or the Google TV app.
