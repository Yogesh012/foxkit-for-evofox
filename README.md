# Foxkit

An unofficial companion app for EvoFox keyboards and mice, for macOS.

Foxkit isn't made by or affiliated with EvoFox. EvoFox is a trademark of its owner.

## What it does

**Menu bar**

- Foxkit lives in the menu bar. Click its icon for a panel with your devices, their connection and battery level, and quick music lighting controls.
- Battery levels come from the device over the USB cable or 2.4 GHz receiver, and from macOS over Bluetooth.
- Closing the main window keeps Foxkit running in the menu bar. **Open Foxkit** in the panel brings the window back, and **Quit Foxkit** ends it.
- **Launch at login** in the panel starts Foxkit in the menu bar when you log in. It's off until you turn it on.

**EvoFox Ronin TKL Wireless keyboard**

- Lighting: effect, brightness, speed, direction and colour preset.
- Music lighting from whatever the Mac is playing, in five styles: Bars, Pulse, Glow, Ripple and Sparkle. They follow your colour preset, brightness and direction, and your normal lighting comes back when they stop.
- Ctrl+Option+M starts and stops the music lighting from anywhere while Foxkit is running, including when it's only in the menu bar.

**EvoFox Banshee 2 mouse**

- DPI levels: switch the active level and edit each level's DPI.
- Report rate and click debounce.

## Supported devices

Tested with one Ronin TKL Wireless and one Banshee 2. Settings are read and changed over the USB cable, and for the keyboard also over its 2.4 GHz receiver. Over Bluetooth, Foxkit shows the connection and battery level only. Other models, and other production batches of these ones, may work differently. If something doesn't look right on yours, please open an issue.

## Requirements

- macOS 14.2 or later, for capturing what the Mac is playing.
- System Audio Recording permission, which macOS asks for the first time music lighting starts.

## Installing

1. Download the `.dmg` from [Releases](https://github.com/Yogesh012/foxkit-for-evofox/releases) and drag Foxkit into Applications.
2. Open Foxkit. Because it isn't signed with an Apple Developer ID, macOS stops it the first time and says it can't check it.
3. Go to **System Settings → Privacy & Security**, scroll down to the message about Foxkit, and click **Open Anyway**. You only need to do this once.

Foxkit checks for a new version shortly after it starts and then once a day, and **Check for updates** in the panel or main window checks straight away. When there is one, **Update to …** appears in the menu bar panel and the main window. Clicking it downloads the update, checks its signature, installs it and restarts Foxkit. Nothing is installed until you click. Versions before 0.2.0 can't update themselves, so install 0.2.0 from Releases once.

macOS may ask for System Audio Recording again after an update, because Foxkit isn't signed with an Apple Developer ID.

## Building from source

Foxkit is a [Tauri](https://tauri.app) app written in Rust, with a small Swift helper for audio capture. You need Rust and the Xcode command line tools.

```sh
tools/audio_tap/build.sh
cargo run --manifest-path app/src-tauri/Cargo.toml
```

To build the app and `.dmg` the way releases are built, install the Tauri CLI once with `cargo install tauri-cli --version "^2" --locked`, then run this from `app/src-tauri`:

```sh
cargo tauri build
```

The results are in `app/src-tauri/target/release/bundle`.

Run the tests with:

```sh
cargo test --manifest-path app/src-tauri/Cargo.toml
```

## Licence

MIT. See [LICENSE](LICENSE).
