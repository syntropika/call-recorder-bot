# Recording operations

The bot starts recording when someone joins the configured voice channel. When the last human participant leaves, the bot disconnects and finalizes the session. Other bots do not keep a session open. Periodic reconciliation also retries disconnects and clears a voice connection left without an active recording. It also finalizes active files on a normal shutdown (`Ctrl+C` or `SIGTERM`) and waits for in-progress transcription.

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

## Discord transcript delivery

After processing succeeds, the bot sends the completed transcript as a `.txt` attachment to the notice channel selected when the recording started. The default is the recorded voice channel's text chat. Changing `/record-setup` while a transcript is processing does not redirect that transcript to another channel. The bot needs **View Channels**, **Send Messages**, and **Attach Files** in the destination.

With S3 storage, delivery occurs after the transcript is archived. Delivery uses the completed text in memory, so local staging cleanup does not remove the attachment before it is sent. Large transcripts are split into numbered UTF-8 attachments of at most 8 MiB. If transcription or archival fails, no completed transcript is posted. If Discord delivery fails, the error is logged and the transcript remains available in storage; delivery is not retried automatically.

## Privacy and retention

Before joining, the bot posts a public notice that separate source tracks are stored. If transcription is enabled, the notice says audio will be sent to an external provider. A failed start notice prevents recording. The bot posts an end notice after leaving. Inform participants about recording, speaker identification, and transcription, and obtain any consent required by your server rules and local law.

Completed transcripts are posted to the configured notice channel and are visible to members who can read that channel. In S3 mode they are also stored in the configured bucket and can be read through the authenticated [MCP endpoint](MCP.md). Protect and delete audio, manifests, and transcripts according to your retention policy. Keep the bucket private.
