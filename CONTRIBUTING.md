# Contributing

Call Recorder Bot is currently focused on Discord capture, S3-compatible storage, speaker-labeled transcription, and authenticated transcript access. Contributions to other capture engines should use the existing ports described in [Architecture](docs/ARCHITECTURE.md).

## Before opening a pull request

1. Describe the behavior you want to change and why. For a new capture engine, explain how it obtains participant identity, records consent notices, and handles channel or call lifecycle.
2. Make the smallest change that covers the behavior. Add a focused test when it protects a real failure mode.
3. Run the commands in [Development](DEVELOP.md). Update documentation and `.env.example` when configuration or operation changes.
4. Keep tokens, deployment URLs, real participant data, audio, transcripts, and local `.env` files out of commits and examples.

Open a pull request with a concise summary, verification evidence, and any deployment or data-handling impact. The pull request triggers verification on the project's runner. Fork PRs may wait for maintainer approval, depending on the repository's GitHub Actions settings. A maintainer should review proposed code and workflow changes before approving execution. Run the Rust checks locally before submitting.

For setup, see [Getting started](docs/GETTING_STARTED.md). For storage and recovery behavior, see [Operations](docs/OPERATIONS.md).
