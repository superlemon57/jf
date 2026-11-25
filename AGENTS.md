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

## Apple TV remote

The iOS side is reverse-engineered and changes between iOS releases. Every exchange
with the iPhone is logged to `apple-tv.log` in the cache folder (`~/.cache/jf/` on
Linux): read it before changing the protocol, and check changes on a real iPhone. A
fake iPhone such as pyatv is useful for regression tests but does not behave like iOS.
