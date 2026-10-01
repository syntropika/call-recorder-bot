# Deployment

The repository includes a multi-stage `Dockerfile` and a GitHub Actions workflow for verification, image publishing, and Coolify deployment. `.dockerignore` excludes local credentials, recordings, and build output from the image context. The image contains the checked-in `config.toml`; runtime credentials are supplied separately.

## Workflow and runner

The [release workflow](../.github/workflows/release.yml) uses a self-hosted runner labeled `self-hosted`, `linux`, `x64`, and `docker`. Pull requests run verification automatically. Pushes to `main` run verification, publishing, and deployment. A repository maintainer can also run the workflow from the **Actions** tab or with:

```bash
gh workflow run release.yml --ref <branch>
```

Runs on a feature branch verify formatting, tests, and Clippy. Only runs on `main` publish and deploy. Documentation-only pushes do not start the release workflow. GitHub requires write access to dispatch a workflow manually.

Each job runs on a fresh, disposable runner container with a private Docker daemon and workspace.
The Rust `container:` on the verify job is optional for runner isolation; it pins the Rust
distribution and lets that job install `libopus-dev` without changing the runner image.
The publish and deploy jobs already check out their own source and do not rely on files from verify.

Before allowing fork PR workflows on a public repository with a privileged Docker-backed runner, select **Require approval for all external contributors** in **Settings → Actions → General**. GitHub's default first-time-contributor policy does not require approval for every external contributor. Review the proposed code and workflow changes before approving a run: approval permits that PR code to execute on the runner. Organization policy can also disable fork workflows on private repositories.

The verify job runs in a Rust container and installs the native Opus build dependencies there. The publish job builds `ghcr.io/<owner>/<repo>:<commit-sha>`. The deploy job starts only after successful verification and publishing.

## Coolify setup

1. Create a Coolify project, a `production` environment, and a server. Put their names in GitHub Actions secrets `COOLIFY_PROJECT` and `COOLIFY_SERVER`.
2. Set `COOLIFY_URL` to the Coolify HTTPS base URL without `/api/v1`. The deploy job reports an error when this secret is absent. The deployment action creates the Docker Image application using slug `call-recorder-bot` when needed; no application UUID is required.
3. Make the GHCR image public, or give the Coolify server a read-only GHCR package token for a private image.
4. Set the application and Coolify secrets below, then run the workflow on `main`.

The [Coolify application manifest](../deployment/coolify/application.json) exposes port 8080, checks `/healthz`, enables consistent container names, and allows a 300-second stop grace period. The workflow sets the HTTPS domain from `MCP_PUBLIC_HOST` without committing the deployment hostname. Coolify keeps a persistent volume at `/app/data` for Discord channel settings and recordings still being processed or retried. Make it writable by container UID 10001. Run one instance per Discord token and data volume.

The final image includes `curl` because Coolify runs the HTTP health check inside the application container. `/healthz` returns `200 OK` without a Bearer token when the MCP HTTP server is running; it does not check Discord connectivity or S3 access. Keep `mcp.enabled = true` and `mcp.listen = "0.0.0.0:8080"` when using this health check. After changing the image, redeploy to recreate the container with the updated dependencies.

## GitHub Actions secrets

Set these in the repository's **Actions secrets**:

| Secret | Purpose |
| --- | --- |
| `DISCORD_TOKEN` | Bot login |
| `S3_BUCKET`, `S3_ENDPOINT` | Bucket and S3-compatible endpoint |
| `S3_ACCESS_KEY_ID`, `S3_SECRET_ACCESS_KEY` | Object read/write credentials |
| `MCP_PUBLIC_HOST` | Public MCP hostname without scheme or path |
| `MCP_ACCESS_TOKEN` | Static Bearer token used by the checked-in config |
| `COOLIFY_URL`, `COOLIFY_PROJECT`, `COOLIFY_SERVER` | Coolify target |
| `COOLIFY_TOKEN` | Coolify API access for reading, configuring, and deploying the application |

Set `TRANSCRIPTION_API_KEY` and `TRANSCRIPTION_URL` to enable transcription. `S3_SESSION_TOKEN` is only for temporary S3 credentials. Create one Coolify API token with `root` permission for the team that owns the application and store it as `COOLIFY_TOKEN`. Coolify's `deploy` permission creates a deploy-only token, so a single token covering the action's read, write, and deploy operations needs `root`. Enable Coolify API access. The workflow's **Check COOLIFY_TOKEN** step reports whether the secret is configured without printing its value.

The action receives these values through step environment variables and syncs them to Coolify at deploy time. They are not added to the image. It checks required runtime values before contacting Coolify and suppresses API response bodies because environment-variable responses can contain sensitive values. A container still needs valid S3 credentials, a bucket, and the configured MCP token to start successfully. Check the Coolify deployment log and then the bot log for the Discord connection.

For OIDC instead of a static MCP token, follow [MCP access](MCP.md) and update the workflow's `required-env-keys` accordingly.
