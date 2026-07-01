//! Contract tests for OAuthQueries trait — InMemoryEngine stub behavior.

use zbrain_core::{
    OAuthQueries, RegisterClientRequest, RegisterClientResponse,
    UpdateClientTtlResponse, RevokeClientResponse,
};
use zbrain_core::InMemoryEngine;

async fn in_memory_oauth() -> Box<dyn OAuthQueries> {
    let engine = InMemoryEngine::default();
    Box::new(engine)
}

#[tokio::test]
async fn register_client_returns_id_and_secret() {
    let queries = in_memory_oauth().await;
    let result = queries
        .register_client(RegisterClientRequest {
            name: "test-agent".into(),
            scope: "read write".into(),
            grant_types: vec!["client_credentials".into()],
            redirect_uris: vec![],
            token_endpoint_auth_method: Some("client_secret_basic".into()),
            token_ttl: Some(3600),
            source_id: "default".into(),
            federated_read: vec![],
        })
        .await;
    assert!(result.is_ok(), "register_client should not error");
    let resp = result.unwrap();
    assert!(!resp.client_id.is_empty(), "client_id must not be empty");
    assert!(!resp.client_secret.is_empty(), "client_secret must not be empty");
}

#[tokio::test]
async fn update_client_ttl_returns_ok() {
    let queries = in_memory_oauth().await;
    let result = queries.update_client_ttl("c1", Some(7200)).await;
    assert!(result.is_ok(), "update_client_ttl should not error");
    let resp = result.unwrap();
    assert!(resp.updated, "updated must be true");
    assert_eq!(resp.token_ttl, Some(7200));
}

#[tokio::test]
async fn update_client_ttl_null_resets() {
    let queries = in_memory_oauth().await;
    let result = queries.update_client_ttl("c1", None).await;
    assert!(result.is_ok());
    let resp = result.unwrap();
    assert!(resp.updated);
    assert_eq!(resp.token_ttl, None);
}

#[tokio::test]
async fn revoke_client_returns_ok() {
    let queries = in_memory_oauth().await;
    let result = queries.revoke_client("c1").await;
    assert!(result.is_ok(), "revoke_client should not error");
    let resp = result.unwrap();
    assert!(resp.revoked, "revoked must be true");
}

#[test]
fn register_client_response_serializes_camel_case() {
    let resp = RegisterClientResponse {
        client_id: "abc123".into(),
        client_secret: "secret-hash".into(),
    };
    let json = serde_json::to_value(&resp).unwrap();
    assert_eq!(json["clientId"], "abc123");
    assert_eq!(json["clientSecret"], "secret-hash");
}

#[test]
fn update_client_ttl_response_serializes_camel_case() {
    let resp = UpdateClientTtlResponse {
        updated: true,
        token_ttl: Some(3600),
    };
    let json = serde_json::to_value(&resp).unwrap();
    assert_eq!(json["updated"], true);
    assert_eq!(json["tokenTtl"], 3600);
}

#[test]
fn revoke_client_response_serializes_camel_case() {
    let resp = RevokeClientResponse { revoked: true };
    let json = serde_json::to_value(&resp).unwrap();
    assert_eq!(json["revoked"], true);
}
