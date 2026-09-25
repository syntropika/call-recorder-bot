# Architecture

One Rust process hosts the capture engine, recording pipeline, S3 archive, and transcript MCP server. The capture engine supplies platform-neutral audio frames and participant identities; storage, processing, and transcript reading sit behind ports.

## Capture engine

`src/engines/mod.rs` defines the capture-engine interface. `src/engines/discord/` implements Discord commands, voice-state handling, notices, Songbird audio capture, and per-server channel settings. The adapter maps each audio source to a participant ID and Discord mention when possible. Sources without a reliable mapping receive an unknown label.

`src/recording.rs` owns the platform-independent lifecycle: it receives PCM frames and source identities, writes individual tracks and a mixed recording, finalizes the session, and sends completed files to processing. Metadata records an engine name, room ID, participant IDs, and labels without depending on Discord types. `src/main.rs` selects the configured engine and connects it to storage and processing.

Only Discord is implemented. Another platform needs its own capture adapter and configuration; the shared recorder should not need platform-specific types.

## Storage and processing

`src/storage/mod.rs` defines `RecordingStorage` and `RecordingWriter`. `src/storage/local.rs` writes mixed and per-source WAV files. `src/storage/s3.rs` archives completed artifacts in an S3-compatible bucket while retaining local working files until processing finishes. Legacy speaker manifests with `ssrc` and numeric `user_id` fields are still readable.

`src/processing/mod.rs` defines `RecordingProcessor`. When a transcription API key is present, `TranscribingProcessor` splits each source track into three-minute chunks, skips silent parts, transcribes chunks, and orders returned segments by their offset in the recording. The transcript header maps `Speaker 1`, `Speaker 2`, and so on to labels from the capture engine. Lines use the form `[MM:SS] Speaker 1: text`. Unmapped sources use a source ID. Older recordings without separate tracks keep an ordinary mixed transcript; participant identities cannot be reconstructed reliably from those files.

`src/transcription/mod.rs` defines the `Transcriber` port. `src/transcription/openai.rs` implements an OpenAI-compatible multipart API with Bearer authentication. Capture continues separately while completed recordings are processed. Each chunk's start and completion are logged.

## Transcript access

`src/mcp/` contains the transcript-reading port and S3 adapter, static-token and OIDC authentication adapters, and the Streamable HTTP transport. It shares S3 settings with the archive path and serves text transcripts from the private bucket. [MCP access](MCP.md) describes the tools and configuration.

For file layout, cleanup, and manual recovery, see [Operations](OPERATIONS.md).
