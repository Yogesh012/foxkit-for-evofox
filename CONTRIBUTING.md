# Contributing to Foxkit

Thanks for helping out. Bug reports, device reports and pull requests are all welcome.

## Reporting a problem

Open an issue and include the device model, its batch if you know it, how it was connected, and your Foxkit and macOS versions. Reports from models or batches Foxkit hasn't been tested with are especially useful.

## Pull requests

1. Fork the repo and make your change on a branch. If you have write access, you can push a branch here instead.
2. Run `cargo fmt` and `cargo test` (see the README for building).
3. Open a pull request against `main`, and say which devices you tried it on.

Pull requests are merged by squashing, once the checks pass.

## Changes that talk to a device

Settings are written to the device's memory, so take care with anything new that's sent to it:

- Try it over the USB cable first, one change at a time.
- Read the current value before writing a new one.
- Never send anything to do with firmware updates, factory resets or clearing macros.
- Say in the pull request exactly what new packets are sent and what they do.

## Licence

By contributing, you agree that your contributions are licensed under the MIT licence in [LICENSE](LICENSE).
