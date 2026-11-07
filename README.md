# Foxkit

An unofficial companion app for EvoFox keyboards and mice, for macOS.

Foxkit isn't made by or affiliated with EvoFox. EvoFox is a trademark of its owner.

## What it does

**EvoFox Ronin TKL Wireless keyboard**

- Lighting: effect, brightness, speed, direction and colour preset.
- Music lighting from whatever the Mac is playing, in five styles: Bars, Pulse, Glow, Ripple and Sparkle. They follow your colour preset, brightness and direction, and your normal lighting comes back when they stop.
- Ctrl+Option+M starts and stops the music lighting from anywhere while Foxkit is open.

**EvoFox Banshee 2 mouse**

- DPI levels: switch the active level and edit each level's DPI.
- Report rate and click debounce.

## Supported devices

Tested with one Ronin TKL Wireless and one Banshee 2, with settings read and changed over the USB cable. Other models, and other production batches of these ones, may work differently. If something doesn't look right on yours, please open an issue.

## Requirements

- macOS 14.2 or later, for capturing what the Mac is playing.
- System Audio Recording permission, which macOS asks for the first time music lighting starts.

## Building from source

Foxkit is a [Tauri](https://tauri.app) app written in Rust, with a small Swift helper for audio capture.

```sh
tools/audio_tap/build.sh
cargo run --manifest-path app/src-tauri/Cargo.toml
```

Run the tests with:

```sh
cargo test --manifest-path app/src-tauri/Cargo.toml
```

## Licence

MIT. See [LICENSE](LICENSE).
