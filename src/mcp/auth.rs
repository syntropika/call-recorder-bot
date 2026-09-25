use std::{
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use axum::http::{HeaderMap, header::AUTHORIZATION};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header, jwk::JwkSet};
use serde::Deserialize;
use subtle::ConstantTimeEq;

use crate::settings::McpAuthSettings;

#[derive(Clone)]
pub enum AuthAdapter {
    Static { token: String },
    Oidc(Arc<OidcVerifier>),
}

pub struct OidcVerifier {
    issuer: String,
    resource_url: String,
    audience: String,
    scope: String,
    jwks_url: reqwest::Url,
    keys: RwLock<JwkSet>,
    client: reqwest::Client,
    last_refresh: tokio::sync::Mutex<Option<Instant>>,
}

#[derive(Deserialize)]
struct Discovery {
    issuer: String,
    jwks_uri: String,
}

#[derive(Deserialize)]
struct Claims {
    #[serde(default)]
    scope: String,
    #[serde(default)]
    permissions: Vec<String>,
}

impl AuthAdapter {
    pub async fn new(settings: &McpAuthSettings) -> Result<Self> {
        match settings {
            McpAuthSettings::StaticToken { token } => {
                let token = token.required()?;
                if token.len() < 32 {
                    bail!("MCP static bearer token must be at least 32 characters");
                }
                Ok(Self::Static { token })
            }
            McpAuthSettings::Oidc {
                issuer,
                audience,
                resource_url,
                scope,
            } => {
                let issuer = issuer.required()?;
                let audience = audience.required()?;
                let resource_url = resource_url.required()?;
                let resource =
                    reqwest::Url::parse(&resource_url).context("invalid OIDC resource URL")?;
                if resource.scheme() != "https"
                    || resource.path() != "/mcp"
                    || resource.query().is_some()
                    || resource.fragment().is_some()
                {
                    bail!("OIDC resource URL must be an HTTPS URL ending in /mcp");
                }
                let parsed = reqwest::Url::parse(&issuer).context("invalid OIDC issuer URL")?;
                if parsed.scheme() != "https" || !parsed.path().ends_with('/') {
                    bail!("OIDC issuer must be an HTTPS URL ending in /");
                }
                let discovery_url = parsed.join(".well-known/openid-configuration")?;
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(10))
                    .build()?;
                let metadata: Discovery = client
                    .get(discovery_url)
                    .send()
                    .await?
                    .error_for_status()?
                    .json()
                    .await?;
                if metadata.issuer != issuer {
                    bail!("OIDC discovery issuer mismatch");
                }
                let jwks_url = reqwest::Url::parse(&metadata.jwks_uri)?;
                if jwks_url.scheme() != "https" {
                    bail!("OIDC JWKS URL must use HTTPS");
                }
                let keys = client
                    .get(jwks_url.clone())
                    .send()
                    .await?
                    .error_for_status()?
                    .json()
                    .await?;
                Ok(Self::Oidc(Arc::new(OidcVerifier {
                    issuer,
                    resource_url,
                    audience,
                    scope: scope.clone(),
                    jwks_url,
                    keys: RwLock::new(keys),
                    client,
                    last_refresh: tokio::sync::Mutex::new(None),
                })))
            }
        }
    }

    pub async fn authorize(&self, headers: &HeaderMap) -> bool {
        let Some(token) = headers
            .get(AUTHORIZATION)
            .and_then(|header| header.to_str().ok())
            .and_then(|header| header.strip_prefix("Bearer "))
        else {
            return false;
        };
        match self {
            Self::Static { token: expected } => {
                token.len() == expected.len()
                    && bool::from(token.as_bytes().ct_eq(expected.as_bytes()))
            }
            Self::Oidc(verifier) => verifier.verify(token).await,
        }
    }

    pub fn issuer(&self) -> Option<&str> {
        match self {
            Self::Static { .. } => None,
            Self::Oidc(verifier) => Some(&verifier.issuer),
        }
    }

    pub fn resource_url(&self) -> Option<&str> {
        match self {
            Self::Static { .. } => None,
            Self::Oidc(verifier) => Some(&verifier.resource_url),
        }
    }

    pub fn scope(&self) -> Option<&str> {
        match self {
            Self::Static { .. } => None,
            Self::Oidc(verifier) => Some(&verifier.scope),
        }
    }
}

impl OidcVerifier {
    async fn verify(&self, token: &str) -> bool {
        let Ok(header) = decode_header(token) else {
            return false;
        };
        if !matches!(header.alg, Algorithm::RS256 | Algorithm::ES256) {
            return false;
        }
        let Some(kid) = header.kid else {
            return false;
        };
        let has_key = self.keys.read().is_ok_and(|keys| keys.find(&kid).is_some());
        if !has_key {
            let mut last_refresh = self.last_refresh.lock().await;
            if !self.keys.read().is_ok_and(|keys| keys.find(&kid).is_some()) {
                if last_refresh
                    .as_ref()
                    .is_some_and(|instant| instant.elapsed() < Duration::from_secs(60))
                {
                    return false;
                }
                *last_refresh = Some(Instant::now());
                let refreshed: Result<JwkSet, _> = async {
                    self.client
                        .get(self.jwks_url.clone())
                        .send()
                        .await?
                        .error_for_status()?
                        .json()
                        .await
                }
                .await;
                let Ok(refreshed) = refreshed else {
                    return false;
                };
                let Ok(mut keys) = self.keys.write() else {
                    return false;
                };
                *keys = refreshed;
            }
        }
        let Ok(keys) = self.keys.read() else {
            return false;
        };
        let Some(jwk) = keys.find(&kid) else {
            return false;
        };
        let Ok(key) = DecodingKey::from_jwk(jwk) else {
            return false;
        };
        let mut validation = Validation::new(header.alg);
        validation.set_issuer(&[&self.issuer]);
        validation.set_audience(&[&self.audience]);
        let Ok(data) = decode::<Claims>(token, &key, &validation) else {
            return false;
        };
        data.claims
            .scope
            .split_whitespace()
            .any(|scope| scope == self.scope)
            || data
                .claims
                .permissions
                .iter()
                .any(|permission| permission == &self.scope)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn rejects_short_public_bearer_keys() -> Result<()> {
        let settings: McpAuthSettings = toml::from_str(
            "provider = \"static_token\"\ntoken = { value = \"known-placeholder\" }",
        )?;
        assert!(AuthAdapter::new(&settings).await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn refreshes_an_unknown_key_immediately_then_limits_retries() -> Result<()> {
        let requests = Arc::new(AtomicUsize::new(0));
        let app = axum::Router::new().route(
            "/jwks",
            axum::routing::get({
                let requests = Arc::clone(&requests);
                move || {
                    let requests = Arc::clone(&requests);
                    async move {
                        requests.fetch_add(1, Ordering::SeqCst);
                        axum::Json(serde_json::json!({ "keys": [] }))
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let verifier = OidcVerifier {
            issuer: "https://issuer.example.com/".into(),
            resource_url: "https://transcripts.example.com/mcp".into(),
            audience: "https://transcripts.example.com/mcp".into(),
            scope: "transcripts:read".into(),
            jwks_url: reqwest::Url::parse(&format!("http://{address}/jwks"))?,
            keys: RwLock::new(JwkSet { keys: vec![] }),
            client: reqwest::Client::new(),
            last_refresh: tokio::sync::Mutex::new(None),
        };
        let token = "eyJhbGciOiJSUzI1NiIsImtpZCI6InJvdGF0ZWQifQ.e30.c2ln";

        assert!(!verifier.verify(token).await);
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert!(!verifier.verify(token).await);
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        server.abort();
        Ok(())
    }
}
