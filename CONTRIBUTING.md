# Contributing

Bug reports and pull requests are welcome. For a larger change, open an issue
first so we can agree on the approach.

## Setup

You need stable Rust. To try your changes against the demo backend:

```sh
cargo run --example demo-server
cargo run -- --target http://localhost:3000
```

## Layout

- `crates/apibreak-core`: the proxy, scenarios, store and mocks. It has no
  terminal dependency and is tested on its own.
- `crates/apibreak`: the CLI and TUI, a thin layer over the core.

Logic belongs in the core wherever possible.

## Before opening a pull request

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Please avoid `unwrap()` outside tests, and add a test with any behavior change.
apibreak aims to stay small with zero setup, so check
[ROADMAP.md](ROADMAP.md) before adding a feature.

## Releasing

Releases are built by [dist](https://github.com/axodotdev/cargo-dist). Update the
version in the root `Cargo.toml`, then push a tag such as `v0.2.0`. Publish
`apibreak-core` to crates.io before `apibreak`, since one depends on the other.
To regenerate `.github/workflows/release.yml` after changing
`dist-workspace.toml`, run `dist generate`.
