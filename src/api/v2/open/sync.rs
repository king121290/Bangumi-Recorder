use axum::{
    Json,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
};
use chrono::NaiveDateTime;
use serde::Serialize;
use sqlx::mysql::MySqlPool;

use crate::api::open::api_token::{
    PERM_READ, PERM_WRITE, api_token_from_request, require_token_with_all_perms,
    require_token_with_perm,
};
use crate::api::v2::response::{ApiResponse, unauthorized};
use crate::api::v2::sync::{SyncRequestBody, SyncResponseData, SyncResponseRecord};
use crate::api::v2::sync_ani::{SyncAniRequest, SyncAniResponse};

#[derive(serde::Deserialize)]
pub struct OpenSyncQuery {
    pub token: Option<String>,
}

#[derive(serde::Deserialize)]
pub struct OpenSyncSinceQuery {
    pub token: Option<String>,
    pub since: Option<NaiveDateTime>,
}

fn forbidden<T: Serialize>(msg: &str) -> (StatusCode, Json<ApiResponse<T>>) {
    (
        StatusCode::FORBIDDEN,
        Json(ApiResponse {
            status: -1,
            data: None,
            message: Some(msg.to_string()),
        }),
    )
}

fn bad_request<T: Serialize>(msg: &str) -> (StatusCode, Json<ApiResponse<T>>) {
    (
        StatusCode::BAD_REQUEST,
        Json(ApiResponse {
            status: -1,
            data: None,
            message: Some(msg.to_string()),
        }),
    )
}

pub async fn sync_records(
    State(pool): State<MySqlPool>,
    headers: HeaderMap,
    Query(query): Query<OpenSyncQuery>,
    Json(body): Json<SyncRequestBody>,
) -> (StatusCode, Json<ApiResponse<SyncResponseData>>) {
    let token = match api_token_from_request(&headers, query.token.as_deref()) {
        Some(t) => t,
        None => return unauthorized("Missing API token"),
    };

    let token_info =
        match require_token_with_all_perms(&pool, Some(token), &[PERM_READ, PERM_WRITE]).await {
            Ok(info) => info,
            Err(StatusCode::UNAUTHORIZED) => return unauthorized("Invalid API token"),
            Err(_) => return forbidden("Insufficient permissions"),
        };

    crate::api::v2::sync::do_sync_records(&pool, token_info.user_id, body).await
}

pub async fn incremental_sync(
    State(pool): State<MySqlPool>,
    headers: HeaderMap,
    Query(query): Query<OpenSyncSinceQuery>,
) -> (StatusCode, Json<ApiResponse<Vec<SyncResponseRecord>>>) {
    let token = match api_token_from_request(&headers, query.token.as_deref()) {
        Some(t) => t,
        None => return unauthorized("Missing API token"),
    };

    let token_info = match require_token_with_perm(&pool, Some(token), &[PERM_READ]).await {
        Ok(info) => info,
        Err(StatusCode::UNAUTHORIZED) => return unauthorized("Invalid API token"),
        Err(_) => return forbidden("Insufficient permissions"),
    };

    let since = match query.since {
        Some(s) => s,
        None => return bad_request("Missing 'since' query parameter"),
    };

    crate::api::v2::sync::do_incremental_sync_records(&pool, token_info.user_id, since).await
}

pub async fn sync_ani_records(
    State(pool): State<MySqlPool>,
    headers: HeaderMap,
    Query(query): Query<OpenSyncQuery>,
    Json(body): Json<SyncAniRequest>,
) -> (StatusCode, Json<ApiResponse<SyncAniResponse>>) {
    let token = match api_token_from_request(&headers, query.token.as_deref()) {
        Some(token) => token,
        None => return unauthorized("Missing API token"),
    };
    let token_info =
        match require_token_with_all_perms(&pool, Some(token), &[PERM_READ, PERM_WRITE]).await {
            Ok(info) => info,
            Err(StatusCode::UNAUTHORIZED) => return unauthorized("Invalid API token"),
            Err(_) => return forbidden("Insufficient permissions"),
        };
    crate::api::v2::sync_ani::do_sync_records(&pool, token_info.user_id, body).await
}
