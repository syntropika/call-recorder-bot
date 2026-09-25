# Recording operations

The bot starts recording when someone joins the configured voice channel and finalizes a session when the channel becomes empty. It also finalizes active files on a normal shutdown (`Ctrl+C` or `SIGTERM`) and waits for in-progress transcription.

## Files and names

With `storage.provider = "local"`, files are kept under `<app.data_dir>/recordings/discord/<guild-id>/`:

- `<recording-name>.wav` is a 48 kHz, 16-bit, mono mixed recording.
- `<recording-name>.source-<source-id>.wav` files hold separate source tracks.
- `<recording-name>.speakers.json` maps tracks to participant IDs and labels and records their offsets from the mixed recording's start.
- `<recording-name>.txt` is the optional speaker-labeled transcript.

New names use the full UTC start time through seconds, for example `2026-09-24T21-35-08Z`. If another recording starts within the same second, its name gets a `-2` suffix. The default `data` directory is Git-ignored. On Unix, recording directories use mode `700` and audio, manifests, and transcripts use mode `600`. Discord channel selections live separately in `<app.data_dir>/config.json`.

## S3 archive and cleanup

With S3 storage, active recordings use `<app.data_dir>/staging/discord/<guild-id>/`. The archive key layout is:

```text
<prefix>/<recording-name>/discord/<guild-id>/<recording-name>.<extension>
```

The checked-in prefix is `recording`. Timestamp folders sort by UTC start time. The bot uploads source tracks, a speaker manifest with `s3://` object locations, and the mixed WAV. If transcription is enabled, it transcribes from local files and uploads the `.txt` afterward. A failed upload or transcription leaves staging files for recovery; incomplete transcripts are not published. After all processing succeeds, local staging cleanup is attempted and a cleanup failure is logged.

The bot does not migrate older local recordings when S3 is enabled. Existing numeric recording names and the earlier directory layout remain accepted by the manual commands.

## Recover a recording

To transcribe or retry a local recording, run:

```bash
cargo run -- --transcribe data/staging/discord/<guild-id>/<recording-name>.wav
```

This reads a speaker manifest when present, writes a sibling `.txt` file, reports completed chunks, and exits without starting the Discord bot. It requires a configured transcription API key.

To retry an S3 archive:

```bash
cargo run -- --archive data/staging/discord/<guild-id>/<recording-name>.wav
```

If transcription previously failed, transcribe the WAV first and then archive it. `--archive` keeps local files in place for inspection. The WAV must be inside a room-ID directory and have a timestamp or legacy recording-ID name.

## Privacy and retention

Before joining, the bot posts a public notice that separate source tracks are stored. If transcription is enabled, the notice says audio will be sent to an external provider. A failed start notice prevents recording. The bot posts an end notice after leaving. Inform participants about recording, speaker identification, and transcription, and obtain any consent required by your server rules and local law.

Transcripts are not posted to Discord. In S3 mode they are stored in the configured bucket and can be read through the authenticated [MCP endpoint](MCP.md). Protect and delete audio, manifests, and transcripts according to your retention policy. Keep the bucket private.
