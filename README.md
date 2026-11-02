# Call Recorder Bot

**Turn Discord voice calls into speaker-labeled transcripts and recordings you control.**

Call Recorder Bot is a self-hosted Rust service that records a configured Discord voice channel, separates participants into source tracks, and can create a timestamped transcript with identified speakers. It archives audio and transcripts in your S3-compatible bucket, including Cloudflare R2. An authenticated MCP endpoint lets other services read the transcripts without making the bucket public.

Discord is the first capture engine. The recording core has ports for other platforms, but Google Meet, Slack, and other engines are not implemented yet.

## From call to archive

1. A server administrator selects a voice channel with `/record-setup`.
2. The bot posts a recording notice, joins when someone enters, and captures a mixed WAV plus separate source tracks.
3. When the channel empties, it finalizes the files and, if configured, transcribes the tracks into one speaker-labeled timeline.
4. It uploads the artifacts to S3-compatible storage, removes local staging files after successful processing, and posts the completed transcript as a text attachment in the configured notice channel. The MCP server can also list and read archived transcripts using authenticated tools.

The bot posts an end notice when it leaves. Failed processing leaves local files available for recovery. Completed transcripts are posted in the configured notice channel; audio remains in storage.

## Start recording

1. Create a Discord bot and an S3-compatible bucket. The checked-in configuration uses S3 storage and enables MCP.
2. Copy `.env.example` to `.env` and set the Discord token, bucket, S3 credentials, endpoint, and an MCP Bearer token. A transcription API key is optional.
3. Run `cargo run --release` on a machine that stays online.
4. In your Discord server, run `/record-setup voice:<voice channel>`.

See [Getting started](docs/GETTING_STARTED.md) for permissions, native dependencies, and the full setup. Review your recording notice and consent requirements before enabling automatic capture.

## Documentation

| Guide | What it covers |
| --- | --- |
| [Getting started](docs/GETTING_STARTED.md) | Install the bot, run it, and choose a Discord channel |
| [Configuration](docs/CONFIGURATION.md) | TOML settings, environment values, S3, and transcription |
| [MCP access](docs/MCP.md) | Transcript tools, Bearer authentication, and optional OIDC |
| [Deployment](docs/DEPLOYMENT.md) | Docker, GitHub Actions, and Coolify |
| [Operations](docs/OPERATIONS.md) | Recording layout, privacy, cleanup, and recovery |
| [Architecture](docs/ARCHITECTURE.md) | Capture engines, storage, and processing ports |
| [Development](DEVELOP.md) | Build and test the Rust service |
| [Contributing](CONTRIBUTING.md) | Propose and prepare changes |

The project is designed for a single running bot instance per Discord token and data volume. Keep the S3 bucket private and set a retention policy that fits your use case.
