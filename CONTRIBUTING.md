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

## Releasing

For maintainers. Versions follow [Semantic Versioning](https://semver.org): a fix raises the last number (0.1.1), and a new feature raises the middle one (0.2.0).

1. On a branch, set `version` in `app/src-tauri/Cargo.toml`, run `cargo check --manifest-path app/src-tauri/Cargo.toml` so `Cargo.lock` picks it up, and merge it through a pull request.
2. Tag the merged commit on `main` and push the tag:

   ```sh
   git switch main
   git pull
   git tag v0.2.0
   git push origin v0.2.0
   ```

3. The **Release** workflow checks the tag matches the version, runs the tests, builds a universal `.dmg` for Apple silicon and Intel Macs, and creates a draft release with notes from the merged pull requests.
4. Download the `.dmg` from the draft, try it, edit the notes if needed, and click **Publish release**.

To build the same universal `.dmg` locally, add the Intel target once with `rustup target add x86_64-apple-darwin`, then run `cargo tauri build --target universal-apple-darwin` from `app/src-tauri`.

## Licence

By contributing, you agree that your contributions are licensed under the MIT licence in [LICENSE](LICENSE).
