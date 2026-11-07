#!/bin/sh
# Builds the helper as its own app, so macOS asks for System Audio Recording for it
# rather than for whatever launched it.
set -e
cd "$(dirname "$0")"
app=FoxkitAudioTap.app
mkdir -p "$app/Contents/MacOS"
cp Info.plist "$app/Contents/Info.plist"
swiftc -O -swift-version 5 audio_tap.swift -o "$app/Contents/MacOS/audio_tap"
codesign --force --sign - "$app"
