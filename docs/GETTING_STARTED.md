# Getting started

Call Recorder Bot currently captures Discord voice channels. It runs as a process on your machine or server; installing the bot in Discord does not host it.

## Requirements

- Rust 1.94 with the edition 2024 toolchain.
- The Opus development library. On Ubuntu, install `libopus-dev`. Building Opus from source instead requires CMake and a C toolchain.
- A Discord application with a bot token, created in the [Discord Developer Portal](https://discord.com/developers/applications).
- An S3-compatible bucket and object read/write credentials for the checked-in configuration. Cloudflare R2 is supported.
- A machine that stays online while calls need to be recorded.

See [Development](../DEVELOP.md) for build commands and [Configuration](CONFIGURATION.md) for local-storage alternatives.

## Invite the bot

1. In the Developer Portal, copy the token from the application's **Bot** page and keep it private.
2. Configure **Guild Install** and generate an install link for your application. Grant **View Channels**, **Connect**, and **Send Messages**.
3. Install it in the Discord server. Check channel overrides: the bot must be able to view and connect to the selected voice channel and send messages in its text chat. If you use another notice channel, grant **Send Messages** there too.

The bot uses the non-privileged `GUILDS` and `GUILD_VOICE_STATES` gateway intents. It does not read message content. It receives commands through Discord's Gateway, so the application's Public Key and an Interactions Endpoint URL are not needed. The bot token is still required.

## Start the process

1. Copy `.env.example` to `.env`. Set `DISCORD_TOKEN`, `S3_BUCKET`, `S3_ENDPOINT`, `S3_ACCESS_KEY_ID`, and `S3_SECRET_ACCESS_KEY`. The checked-in config also enables MCP, so set `MCP_PUBLIC_HOST` and a random `MCP_ACCESS_TOKEN` of at least 32 characters, or disable MCP in `config.toml`.
2. To enable transcription, set `TRANSCRIPTION_API_KEY` and `TRANSCRIPTION_URL`. Leave the API key unset to record without transcribing.
3. Adjust `config.toml` for your storage provider and run:

   ```bash
   cargo run --release
   ```

The bot loads `.env` from its working directory and reads `config.toml` at startup. Restart it after configuration changes. See [Configuration](CONFIGURATION.md) for all settings.

## Choose a voice channel

Run this command in the Discord server as a member with **Manage Server** permission:

```text
/record-setup voice:<voice channel>
```

The bot posts notices in the voice channel's text chat by default. The optional `notices` argument selects another text or voice channel. Use `/record-status` to inspect the setup and `/record-disable` to stop automatic recording.

Before enabling automatic capture, tell participants about recording, separate speaker tracks, and transcription. The bot posts a notice before joining and an end notice after leaving. A failed start notice prevents recording. Follow your server rules and applicable consent requirements. [Operations](OPERATIONS.md) explains where recordings are kept and how failed jobs are recovered.
