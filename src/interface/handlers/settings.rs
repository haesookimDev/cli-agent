//! `/v1/settings`, `/v1/models`, `/v1/providers`, `/v1/local-models` — read
//! and patch AppSettings, list the model catalog, query a vLLM server, and
//! toggle model/provider enablement.

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;

use crate::interface::api::{json_value, ApiState};

#[derive(Debug, Deserialize)]
pub(crate) struct VllmModelsQuery {
    pub url: String,
}

pub(crate) async fn get_settings_handler(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, &[]) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }
    let settings = state.orchestrator.current_settings().await;
    (StatusCode::OK, Json(json_value(settings)))
}

pub(crate) async fn update_settings_handler(
    State(state): State<ApiState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, body.as_ref()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }

    let patch = match serde_json::from_slice::<crate::types::SettingsPatch>(body.as_ref()) {
        Ok(v) => v,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": err.to_string()})),
            );
        }
    };

    state.orchestrator.update_settings(patch).await;
    let settings = state.orchestrator.current_settings().await;
    (StatusCode::OK, Json(json_value(settings)))
}

pub(crate) async fn list_models_handler(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, &[]) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }

    let router = state.orchestrator.router();
    let preferred = router.preferred_model();
    let disabled_providers = router.disabled_providers();
    let models: Vec<serde_json::Value> = router
        .catalog()
        .iter()
        .map(|spec| {
            serde_json::json!({
                "spec": spec,
                "enabled": !router.is_model_disabled(&spec.model_id)
                    && !disabled_providers.contains(&spec.provider),
                "is_preferred": preferred.as_deref() == Some(spec.model_id.as_str()),
            })
        })
        .collect();

    (StatusCode::OK, Json(serde_json::json!(models)))
}

pub(crate) async fn list_vllm_models_handler(
    State(state): State<ApiState>,
    Query(query): Query<VllmModelsQuery>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, &[]) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }

    let endpoint = format!("{}/v1/models", query.url.trim_end_matches('/'));
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(_) => reqwest::Client::new(),
    };

    match client.get(&endpoint).send().await {
        Ok(resp) if resp.status().is_success() => {
            let body: serde_json::Value = resp.json().await.unwrap_or_default();
            let model_ids: Vec<String> = body["data"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|m| m["id"].as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            (StatusCode::OK, Json(serde_json::json!(model_ids)))
        }
        Ok(resp) => {
            let status = resp.status();
            (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({"error": format!("vLLM returned {}", status)})),
            )
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

pub(crate) async fn toggle_model_handler(
    State(state): State<ApiState>,
    Path(model_id): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, &[]) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }

    let router = state.orchestrator.router();
    let currently_disabled = router.is_model_disabled(&model_id);
    router.set_model_disabled(&model_id, !currently_disabled);
    state.orchestrator.persist_current_settings().await;
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "model_id": model_id,
            "enabled": currently_disabled,
        })),
    )
}

pub(crate) async fn toggle_provider_handler(
    State(state): State<ApiState>,
    Path(provider_name): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, &[]) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }

    let pk = match serde_json::from_value::<crate::router::ProviderKind>(serde_json::Value::String(
        provider_name.clone(),
    )) {
        Ok(v) => v,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "unknown provider"})),
            );
        }
    };

    let router = state.orchestrator.router();
    let currently_disabled = router.disabled_providers().contains(&pk);
    router.set_provider_disabled(pk, !currently_disabled);
    state.orchestrator.persist_current_settings().await;
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "provider": provider_name,
            "enabled": currently_disabled,
        })),
    )
}
