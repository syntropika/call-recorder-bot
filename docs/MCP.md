# Transcript MCP

The Rust process also serves a Streamable HTTP MCP endpoint at `https://<MCP_PUBLIC_HOST>/mcp`. It reads speaker-labeled `.txt` objects from the configured private S3-compatible bucket. MCP requires `storage.provider = "s3"`; set `mcp.enabled = false` when using only local storage.

The available tools are:

| Tool | Result |
| --- | --- |
| `list_transcripts` | Recording identifiers and an optional `next_cursor` |
| `get_transcript` | The speaker-labeled text for a recording selected by engine, room ID, and recording ID |

The tools do not expose WAV files or speaker manifests. An S3 listing page can contain no transcripts when it contains only audio objects; follow `next_cursor` to inspect later pages. The server also exposes a public `/healthz` endpoint that reports process availability only.

## Static Bearer token

The checked-in `[mcp.auth]` adapter expects an `Authorization: Bearer <token>` header. Set `MCP_ACCESS_TOKEN` to a random value of at least 32 characters, for example from `openssl rand -hex 32`. The token has no built-in expiry; rotate it by updating the runtime secret and restarting the process.

`[mcp].allowed_hosts` reads `MCP_PUBLIC_HOST` in the checked-in config and accepts that hostname for the MCP transport. Set the hostname without `https://` or `/mcp`. Loopback hosts are allowed for local checks.

Configure an MCP client with the `/mcp` URL and Bearer token. For human inspection, [MCP Inspector](https://modelcontextprotocol.io/docs/tools/inspector) can list tool schemas and invoke tools when given the same authentication header.

## OIDC adapter

To use an OIDC provider such as Auth0, replace the checked-in `[mcp.auth]` table with:

```toml
[mcp.auth]
provider = "oidc"
issuer = { value = "https://auth.example.com/" }
audience = { value = "https://transcripts.example.com/mcp" }
resource_url = { value = "https://transcripts.example.com/mcp" }
scope = "transcripts:read"
```

Register an API with the chosen audience and grant clients the `transcripts:read` scope. Store OAuth client credentials in each client's secret store. The adapter validates JWT signature, issuer, audience, expiration, and scope or permissions against the provider's JWKS. Its protected-resource metadata is available at `/.well-known/oauth-protected-resource/mcp`.

`resource_url` is needed only for OIDC discovery. `issuer`, `audience`, and `resource_url` also accept `{ env = "NAME" }` sources when their actual values belong in runtime configuration. If deploying with OIDC, remove `MCP_ACCESS_TOKEN` from the workflow's `required-env-keys` and do not supply a static-token config field.
