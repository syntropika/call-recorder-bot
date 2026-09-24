mod auth;
mod store;

use std::{future::Future, net::SocketAddr, sync::Arc};

use anyhow::{Context, Result};
use axum::{
    Router,
    extract::State,
    http::{HeaderMap, HeaderValue, Request, StatusCode, header::WWW_AUTHENTICATE},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig},
    schemars, tool, tool_handler, tool_router,
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    },
};
use serde::Deserialize;

use crate::settings::{McpSettings, S3Settings};
use auth::AuthAdapter;
use store::{S3TranscriptStore, TranscriptRef, TranscriptStore};

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListArgs {
    cursor: Option<String>,
    #[serde(default = "default_limit")]
    limit: i32,
}

fn default_limit() -> i32 {
    100
}

#[derive(Clone)]
struct TranscriptMcp {
    store: Arc<dyn TranscriptStore>,
}

#[tool_router]
impl TranscriptMcp {
    fn new(store: Arc<dyn TranscriptStore>) -> Self {
        Self { store }
    }

    #[tool(
        description = "List archived speaker-labeled transcripts. Follow next_cursor for later pages; an S3 page may contain no transcripts when it contains only audio objects."
    )]
    async fn list_transcripts(
        &self,
        Parameters(args): Parameters<ListArgs>,
    ) -> Result<CallToolResult, McpError> {
        if !(1..=1000).contains(&args.limit) {
            return Ok(CallToolResult::error(vec![ContentBlock::text(
                "limit must be between 1 and 1000",
            )]));
        }
        match self.store.list(args.cursor, args.limit).await {
            Ok(page) => Ok(CallToolResult::structured(
                serde_json::to_value(page)
                    .expect("TranscriptPage contains only JSON-compatible values"),
            )),
            Err(error) => {
                tracing::error!(%error, "could not list transcripts");
                Ok(CallToolResult::error(vec![ContentBlock::text(
                    "Could not list transcripts",
                )]))
            }
        }
    }

    #[tool(
        description = "Read one speaker-labeled transcript by the engine, room_id, and recording returned by list_transcripts."
    )]
    async fn get_transcript(
        &self,
        Parameters(reference): Parameters<TranscriptRef>,
    ) -> Result<CallToolResult, McpError> {
        match self.store.get(&reference).await {
            Ok(Some(transcript)) => Ok(CallToolResult::success(vec![ContentBlock::text(
                transcript,
            )])),
            Ok(None) => Ok(CallToolResult::error(vec![ContentBlock::text(
                "Transcript not found",
            )])),
            Err(error) => {
                tracing::error!(%error, "could not read transcript");
                Ok(CallToolResult::error(vec![ContentBlock::text(
                    "Could not read transcript",
                )]))
            }
        }
    }
}

#[tool_handler]
impl ServerHandler for TranscriptMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::from_build_env())
            .with_instructions("Read archived speaker-labeled call transcripts.".to_owned())
    }
}

async fn authorize(
    State(state): State<AuthState>,
    headers: HeaderMap,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    if state.adapter.authorize(&headers).await {
        return next.run(request).await;
    }
    let mut response = (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    if let Ok(challenge) = HeaderValue::from_str(&state.challenge) {
        response.headers_mut().insert(WWW_AUTHENTICATE, challenge);
    }
    response
}

#[derive(Clone)]
struct AuthState {
    adapter: AuthAdapter,
    challenge: String,
}

fn router(auth: AuthAdapter, store: Arc<dyn TranscriptStore>, allowed_hosts: &[String]) -> Router {
    let hosts = allowed_hosts
        .iter()
        .cloned()
        .chain(["localhost".into(), "127.0.0.1".into()])
        .collect::<Vec<_>>();
    let service = StreamableHttpService::new(
        move || Ok(TranscriptMcp::new(store.clone())),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default()
            .with_legacy_session_mode(false)
            .with_json_response(true)
            .with_allowed_hosts(hosts),
    );
    let metadata = auth
        .issuer()
        .zip(auth.resource_url())
        .map(|(issuer, resource)| {
            serde_json::json!({
                "resource": resource,
                "authorization_servers": [issuer],
                "scopes_supported": auth.scope().into_iter().collect::<Vec<_>>()
            })
        });
    let challenge = if let Some(resource_url) = auth.resource_url() {
        let mut metadata_url =
            reqwest::Url::parse(resource_url).expect("validated OIDC resource URL");
        metadata_url.set_path("/.well-known/oauth-protected-resource/mcp");
        format!("Bearer resource_metadata=\"{metadata_url}\"")
    } else {
        "Bearer".to_owned()
    };
    let protected =
        Router::new()
            .nest_service("/mcp", service)
            .layer(middleware::from_fn_with_state(
                AuthState {
                    adapter: auth,
                    challenge,
                },
                authorize,
            ));
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(move || {
                let metadata = metadata.clone();
                async move {
                    match metadata {
                        Some(metadata) => (StatusCode::OK, axum::Json(metadata)).into_response(),
                        None => StatusCode::NOT_FOUND.into_response(),
                    }
                }
            }),
        )
        .merge(protected)
}

pub async fn start(
    settings: &McpSettings,
    s3: &S3Settings,
) -> Result<impl Future<Output = Result<()>> + use<>> {
    let auth = AuthAdapter::new(&settings.auth).await?;
    let store: Arc<dyn TranscriptStore> = S3TranscriptStore::new(s3)?;
    let allowed_hosts = settings.resolved_allowed_hosts()?;
    let app = router(auth, store, &allowed_hosts);
    let address: SocketAddr = settings.listen.parse()?;
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .context("could not bind MCP listener")?;
    tracing::info!(%address, "transcript MCP listening");
    Ok(async move {
        axum::serve(listener, app)
            .await
            .context("MCP HTTP server failed")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use store::TranscriptPage;

    struct FakeStore;

    #[async_trait::async_trait]
    impl TranscriptStore for FakeStore {
        async fn list(&self, _: Option<String>, _: i32) -> Result<TranscriptPage> {
            Ok(TranscriptPage {
                items: vec![TranscriptRef {
                    engine: "discord".into(),
                    room_id: "42".into(),
                    recording: "2026-09-24T21-35-08Z".into(),
                }],
                next_cursor: None,
            })
        }
        async fn get(&self, _: &TranscriptRef) -> Result<Option<String>> {
            Ok(Some("[00:01] Speaker 1: Hello".into()))
        }
    }

    #[tokio::test]
    async fn tool_reads_from_store() -> Result<()> {
        let server = TranscriptMcp::new(Arc::new(FakeStore));
        let result = server
            .list_transcripts(Parameters(ListArgs {
                cursor: None,
                limit: 100,
            }))
            .await?;
        assert!(
            result
                .structured_content
                .unwrap()
                .to_string()
                .contains("2026-09-24T21-35-08Z")
        );
        Ok(())
    }

    #[tokio::test]
    async fn http_requires_bearer_and_serves_mcp_tools() -> Result<()> {
        let auth = AuthAdapter::Static {
            token: "expected-token".into(),
        };
        let app = router(auth, Arc::new(FakeStore), &[]);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let client = reqwest::Client::new();
        let endpoint = format!("http://{address}/mcp");
        let body = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}});
        let unauthorized = client
            .post(&endpoint)
            .header("Host", "localhost")
            .json(&body)
            .send()
            .await?;
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
        let wrong_token = client
            .post(&endpoint)
            .header("Host", "localhost")
            .header("Authorization", "Bearer wrong-token")
            .json(&body)
            .send()
            .await?;
        assert_eq!(wrong_token.status(), StatusCode::UNAUTHORIZED);
        let response = client
            .post(&endpoint)
            .header("Host", "localhost")
            .header("Authorization", "Bearer expected-token")
            .header("Accept", "application/json, text/event-stream")
            .json(&body)
            .send()
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let text = response.text().await?;
        assert!(text.contains("list_transcripts"), "{text}");
        assert!(text.contains("get_transcript"), "{text}");
        let read = client.post(&endpoint).header("Host", "localhost")
            .header("Authorization", "Bearer expected-token")
            .header("Accept", "application/json, text/event-stream")
            .json(&serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"get_transcript","arguments":{"engine":"discord","room_id":"42","recording":"2026-09-24T21-35-08Z"}}}))
            .send().await?;
        assert_eq!(read.status(), StatusCode::OK);
        assert!(read.text().await?.contains("Speaker 1: Hello"));
        server.abort();
        Ok(())
    }
}
