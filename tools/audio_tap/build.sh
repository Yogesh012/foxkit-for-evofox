#!/bin/sh
# Builds the audio helper as a small app, for both Apple silicon and Intel Macs. Packaging
# puts it inside Foxkit.app, and running from source uses it from here.
set -e
cd "$(dirname "$0")"
app=FoxkitAudioTap.app
minimum=14.2
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

mkdir -p "$app/Contents/MacOS"
cp Info.plist "$app/Contents/Info.plist"
for arch in arm64 x86_64; do
    swiftc -O -swift-version 5 -target "$arch-apple-macos$minimum" audio_tap.swift -o "$work/audio_tap-$arch"
done
lipo -create "$work/audio_tap-arm64" "$work/audio_tap-x86_64" -output "$app/Contents/MacOS/audio_tap"
codesign --force --sign - "$app"
