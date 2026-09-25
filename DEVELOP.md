# Development

## Local setup

Use Rust 1.94.0 with `rustfmt` and `clippy`. On Ubuntu, install the native Opus development library, CMake, and pkg-config:

```bash
sudo apt-get install cmake libopus-dev pkg-config
rustup toolchain install 1.94.0 --profile minimal --component rustfmt --component clippy
```

Run `cargo +1.94.0 build --locked` to build the bot. For local operation, copy `.env.example` to the Git-ignored `.env` and follow [Getting started](docs/GETTING_STARTED.md). Unit tests use local fixtures and do not need live Discord, S3, or transcription credentials. A live Discord voice test needs a bot token and a test server.

## Verification

Run the same checks as the release workflow:

```bash
cargo +1.94.0 fmt --check
cargo +1.94.0 test --locked
cargo +1.94.0 clippy --locked --all-targets -- -D warnings
```

The workflow can be checked with `actionlint`. Its custom `docker` runner label is declared in `.github/actionlint.yaml`.

Pull requests verify automatically on the self-hosted runner, subject to the repository's fork approval settings. Maintainers can also verify a branch with `gh workflow run release.yml --ref <branch>`. A feature-branch run verifies code only; a run on `main` also publishes and may deploy. GitHub requires write access to dispatch it.

## Where to change code

- [Architecture](docs/ARCHITECTURE.md) maps capture, storage, transcription, and MCP modules.
- [Configuration](docs/CONFIGURATION.md) covers settings and runtime sources.
- [Operations](docs/OPERATIONS.md) describes file layout and recovery behavior.

Keep platform APIs inside a capture adapter. Shared recording metadata and processing should use platform-neutral IDs and labels. If a change affects a runtime setting or an operator procedure, update the corresponding guide and `.env.example` when applicable.
