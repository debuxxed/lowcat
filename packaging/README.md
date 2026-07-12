# Lowcat Packaging

Lowcat uses `cargo-packager` for OS-specific app packages. The bundle metadata
lives in `Cargo.toml` under `[package.metadata.packager]`.

Install the Cargo subcommand once:

```sh
cargo install cargo-packager
```

Install CMake so the bundled Opus decoder can be compiled:

```sh
brew install cmake
```

Build a normal release binary:

```sh
cargo build --release
```

Build a macOS `.app` bundle on macOS:

```sh
cargo-packager --release --formats app
```

Verified output path on macOS:

```text
target/release/Lowcat.app
```

Build a macOS `.dmg` on macOS:

```sh
cargo-packager --release --formats dmg
```

Expected output is under:

```text
target/release/Lowcat_0.1.0_aarch64.dmg
```

`cargo-packager` does not sign, notarize, or publish releases for you. It creates
the platform package structure; signing and release upload can be added later in
CI when the certificate and release flow are known.

Lowcat does not bundle `ffmpeg` or `ffprobe`. They must be available in `PATH`
at runtime.
