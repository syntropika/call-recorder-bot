# Configuration

The process reads `config.toml` from its working directory at startup. Application choices live in that file; credentials and deployment-specific values can come from the process environment or an ignored local `.env` file. Restart the process after changing either source.

## Value sources

Fields that accept a runtime value use one of these TOML forms:

```toml
endpoint = { env = "S3_ENDPOINT" }
# Or, for a public fixed value:
endpoint = { value = "https://s3.example.com" }
```

Specify exactly one of `env` or `value`. An `env` source reads the named variable at runtime and fails if a required value is missing. Avoid committing credentials as literal values. The process calls `dotenvy` on startup, so values from a local `.env` file are available when the process environment does not already supply them.

`app.data_dir` is resolved relative to the config file when it is not absolute. `logging.filter` uses Rust's tracing filter syntax, such as `info` or `call_recorder_bot=debug`.

## Capture and storage

`engine.provider = "discord"` selects the only implemented capture engine. `engine.discord.token` reads `DISCORD_TOKEN` in the checked-in config. Discord voice and notice channels are selected with slash commands and stored in `<app.data_dir>/config.json`.

The checked-in config selects `storage.provider = "s3"`. To store files only on disk, set `storage.provider = "local"` and `mcp.enabled = false`. Local recordings remain in `<app.data_dir>/recordings`. Turning on S3 later does not migrate existing local recordings.

The `[storage.s3]` table accepts:

| Field | Purpose |
| --- | --- |
| `bucket` | Bucket name; the checked-in config reads `S3_BUCKET` |
| `region` | AWS region, or `auto` for Cloudflare R2 |
| `endpoint` | S3-compatible endpoint; omit this field for AWS S3 |
| `access_key_id`, `secret_access_key` | Object read/write credentials |
| `session_token` | Optional third value for temporary S3 credentials |
| `prefix` | Object-key prefix; `recording` in the checked-in config |
| `force_path_style` | Path-style requests; `true` by default |

For R2, use `region = "auto"` and an endpoint in the form `https://<account-id>.r2.cloudflarestorage.com`. Set `force_path_style = false` if another provider requires virtual-hosted addressing. Uploads use standard S3 operations, including multipart upload for large WAV files. Keep the bucket private.

The normal long-lived R2 API credential pair does not need `S3_SESSION_TOKEN`. Set it only when your provider issues temporary credentials with a session token. Temporary credentials expire; this process does not automatically renew them.

## Transcription

`[transcription].api_key` is optional. When its source is absent, the bot records and archives audio without transcription. When it is present, `[transcription].url` must resolve to an OpenAI-compatible multipart transcription endpoint. The `model` and `language` are configured in TOML; `language` describes the input language, not a translation request.

The current config uses `model = "large-v3-v20240930_626MB"` and `language = "en"`. To use another compatible provider, set its URL and supported model. Each non-silent chunk is sent as a WAV `file` with `model` and `language` fields and Bearer authentication. [Architecture](ARCHITECTURE.md) describes how the speaker tracks become one timeline.

## Runtime values

`.env.example` lists the local variables. The checked-in config uses these names:

| Variable | When needed |
| --- | --- |
| `DISCORD_TOKEN` | Always with the Discord engine |
| `S3_BUCKET`, `S3_ACCESS_KEY_ID`, `S3_SECRET_ACCESS_KEY` | S3 storage and manual archive |
| `S3_ENDPOINT` | The checked-in R2 configuration |
| `S3_SESSION_TOKEN` | Only with temporary S3 credentials |
| `TRANSCRIPTION_API_KEY`, `TRANSCRIPTION_URL` | To enable transcription |
| `MCP_PUBLIC_HOST`, `MCP_ACCESS_TOKEN` | The checked-in static-token MCP configuration |

MCP can also use OIDC instead of a static token. [MCP access](MCP.md) covers that configuration. The old `DATA_DIR`, `RUST_LOG`, `STORAGE_BACKEND`, `S3_REGION`, `S3_PREFIX`, `S3_FORCE_PATH_STYLE`, `TRANSCRIPTION_MODEL`, and `TRANSCRIPTION_LANGUAGE` environment settings are not read; change the corresponding TOML fields instead.
