//! HTTP-level integration tests for `interface::api::router`.
//!
//! These tests bypass the network: they construct an in-process `axum::Router`
//! against a freshly-bootstrapped `ApiState` (SQLite-backed `MemoryManager`,
//! no LLM router actually called), then drive it via `tower::ServiceExt`. The
//! goal is to cover the auth + handler glue without spinning real models.

use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::http::{HeaderValue, Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;
use uuid::Uuid;

use cli_agent::agents::AgentRegistry;
use cli_agent::context::ContextManager;
use cli_agent::interface::api::{router, ApiState};
use cli_agent::mcp::McpRegistry;
use cli_agent::memory::MemoryManager;
use cli_agent::orchestrator::cluster::OrchestratorCluster;
use cli_agent::orchestrator::coder_backend::{CoderSessionManager, LlmCoderBackend};
use cli_agent::orchestrator::Orchestrator;
use cli_agent::router::ModelRouter;
use cli_agent::runtime::AgentRuntime;
use cli_agent::session_workspace::SessionWorkspaceManager;
use cli_agent::terminal::TerminalManager;
use cli_agent::types::{CoderBackendKind, RepoAnalysisConfig, ValidationConfig};
use cli_agent::webhook::{AuthManager, WebhookDispatcher};

const TEST_API_KEY: &str = "test-key";
const TEST_API_SECRET: &str = "test-secret";

struct TestHarness {
    state: ApiState,
    auth: Arc<AuthManager>,
    _tmp: std::path::PathBuf,
}

async fn make_harness() -> TestHarness {
    let tmp = std::env::temp_dir().join(format!("api-it-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&tmp).unwrap();

    let memory = Arc::new(
        MemoryManager::new(
            tmp.join("sessions"),
            format!("sqlite://{}", tmp.join("db.sqlite").display()).as_str(),
        )
        .await
        .unwrap(),
    );

    let auth = Arc::new(AuthManager::new(TEST_API_KEY, TEST_API_SECRET, 300));
    let webhook = Arc::new(WebhookDispatcher::new(
        memory.clone(),
        auth.clone(),
        Duration::from_secs(1),
    ));

    let router = Arc::new(ModelRouter::new(
        "http://127.0.0.1:1",
        None,
        None,
        None,
        None,
    ));

    let mut coder_mgr =
        CoderSessionManager::new(memory.clone(), std::path::PathBuf::new(), CoderBackendKind::Llm);
    coder_mgr.register_backend(Arc::new(LlmCoderBackend::new(router.clone())));

    let session_workspace = SessionWorkspaceManager::new(tmp.join("workspace"));

    let orchestrator = Orchestrator::new(
        AgentRuntime::new(2),
        AgentRegistry::builtin(),
        router,
        memory.clone(),
        Arc::new(ContextManager::new(8_000)),
        webhook,
        4,
        Arc::new(McpRegistry::new()),
        Arc::new(coder_mgr),
        Arc::new(ValidationConfig::default()),
        Arc::new(RepoAnalysisConfig::default()),
        session_workspace.clone(),
        None,
    );

    let terminal = TerminalManager::new(session_workspace);
    let cluster = OrchestratorCluster::new();

    TestHarness {
        state: ApiState {
            orchestrator,
            auth: auth.clone(),
            terminal,
            cluster,
        },
        auth,
        _tmp: tmp,
    }
}

fn signed_request(
    auth: &AuthManager,
    method: &str,
    path: &str,
    body: &[u8],
) -> Request<Body> {
    let timestamp = chrono::Utc::now().timestamp().to_string();
    let nonce = format!("nonce-{}", Uuid::new_v4());
    let signature = auth.sign(&timestamp, &nonce, body).unwrap();

    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("X-API-Key", TEST_API_KEY)
        .header("X-Timestamp", &timestamp)
        .header("X-Nonce", &nonce)
        .header("X-Signature", &signature);
    if !body.is_empty() {
        req = req.header("Content-Type", "application/json");
    }
    req.body(Body::from(body.to_vec())).unwrap()
}

async fn read_json(body: axum::body::Body) -> serde_json::Value {
    let bytes: Bytes = body.collect().await.unwrap().to_bytes();
    if bytes.is_empty() {
        return serde_json::Value::Null;
    }
    serde_json::from_slice(&bytes).unwrap_or_else(|_| serde_json::Value::Null)
}

#[tokio::test]
async fn unauthenticated_request_returns_401() {
    let harness = make_harness().await;
    let app = router(harness.state);

    let req = Request::builder()
        .method("GET")
        .uri("/v1/runs/active")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn create_then_list_session_roundtrip() {
    let harness = make_harness().await;
    let app = router(harness.state);

    let create = signed_request(&harness.auth, "POST", "/v1/sessions", b"");
    let resp = app.clone().oneshot(create).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = read_json(resp.into_body()).await;
    let session_id = body
        .get("session_id")
        .and_then(|v| v.as_str())
        .expect("response carries session_id");
    let session_id = Uuid::parse_str(session_id).expect("session_id is a uuid");

    let list = signed_request(&harness.auth, "GET", "/v1/sessions?limit=10", b"");
    let resp = app.clone().oneshot(list).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp.into_body()).await;
    let arr = body.as_array().expect("list returns an array");
    assert!(
        arr.iter().any(|s| s
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(|s| s == session_id.to_string())
            .unwrap_or(false)),
        "newly created session must appear in list"
    );

    let path = format!("/v1/sessions/{}", session_id);
    let get_one = signed_request(&harness.auth, "GET", &path, b"");
    let resp = app.clone().oneshot(get_one).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn get_unknown_session_returns_404() {
    let harness = make_harness().await;
    let app = router(harness.state);

    let path = format!("/v1/sessions/{}", Uuid::new_v4());
    let req = signed_request(&harness.auth, "GET", &path, b"");
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn invalid_session_uuid_returns_400() {
    let harness = make_harness().await;
    let app = router(harness.state);

    let req = signed_request(&harness.auth, "GET", "/v1/sessions/not-a-uuid", b"");
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn replay_nonce_is_rejected() {
    let harness = make_harness().await;
    let app = router(harness.state);

    let timestamp = chrono::Utc::now().timestamp().to_string();
    let nonce = format!("replay-{}", Uuid::new_v4());
    let signature = harness.auth.sign(&timestamp, &nonce, b"").unwrap();
    let build = || {
        Request::builder()
            .method("GET")
            .uri("/v1/sessions?limit=1")
            .header("X-API-Key", TEST_API_KEY)
            .header(
                "X-Timestamp",
                HeaderValue::from_str(timestamp.as_str()).unwrap(),
            )
            .header("X-Nonce", HeaderValue::from_str(nonce.as_str()).unwrap())
            .header(
                "X-Signature",
                HeaderValue::from_str(signature.as_str()).unwrap(),
            )
            .body(Body::empty())
            .unwrap()
    };

    let resp = app.clone().oneshot(build()).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = app.oneshot(build()).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn list_active_runs_empty_returns_array() {
    let harness = make_harness().await;
    let app = router(harness.state);

    let req = signed_request(&harness.auth, "GET", "/v1/runs/active", b"");
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp.into_body()).await;
    assert!(
        body.as_array().is_some_and(|a| a.is_empty()),
        "no active runs yet"
    );
}

#[tokio::test]
async fn register_then_list_webhook() {
    let harness = make_harness().await;
    let app = router(harness.state);

    let payload = serde_json::json!({
        "url": "https://example.com/hook",
        "events": ["run.started", "run.completed"],
        "secret": "shh",
    });
    let body_bytes = serde_json::to_vec(&payload).unwrap();

    let req = signed_request(
        &harness.auth,
        "POST",
        "/v1/webhooks/endpoints",
        &body_bytes,
    );
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let req = signed_request(&harness.auth, "GET", "/v1/webhooks/endpoints", b"");
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp.into_body()).await;
    let arr = body.as_array().expect("array response");
    assert!(
        arr.iter().any(|w| w
            .get("url")
            .and_then(|v| v.as_str())
            .map(|s| s == "https://example.com/hook")
            .unwrap_or(false)),
        "registered webhook is listed"
    );
}

#[tokio::test]
async fn list_skills_returns_array() {
    let harness = make_harness().await;
    let app = router(harness.state);

    let req = signed_request(&harness.auth, "GET", "/v1/skills", b"");
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp.into_body()).await;
    assert!(body.as_array().is_some(), "skills response is an array");
}
