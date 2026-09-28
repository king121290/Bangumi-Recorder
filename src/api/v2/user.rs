use axum::{
    Json,
    extract::{Extension, State},
    http::{HeaderMap, StatusCode},
};
use serde::Serialize;
use sqlx::mysql::MySqlPool;

use super::response::{ApiResponse, bad_request, internal_error, success, success_empty};
use crate::api::logs::{operation_metadata, write_system_log};
use crate::api::user::{
    UpdatePasswordRequest, UpdateUserInfo, UserInfo, get_info as v1_get_info,
    update_info as v1_update_info, update_password as v1_update_password,
};
use crate::auth_bearer::AuthUser;

async fn username_for_log(pool: &MySqlPool, user_id: i64) -> String {
    sqlx::query_scalar::<_, String>("SELECT username FROM users WHERE id = ?")
        .bind(user_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| format!("user#{}", user_id))
}

#[derive(Serialize)]
pub struct TokenData {
    pub api_token: String,
}

pub async fn get_info(
    State(pool): State<MySqlPool>,
    Extension(auth_user): Extension<AuthUser>,
) -> (StatusCode, Json<ApiResponse<UserInfo>>) {
    let v1_resp = v1_get_info(State(pool.clone()), Extension(auth_user)).await;
    success(v1_resp.0)
}

pub async fn update_info(
    State(pool): State<MySqlPool>,
    Extension(auth_user): Extension<AuthUser>,
    Json(payload): Json<UpdateUserInfo>,
) -> (StatusCode, Json<ApiResponse<()>>) {
    let v1_resp = v1_update_info(State(pool.clone()), Extension(auth_user), Json(payload)).await;
    let inner = v1_resp.0;
    if inner.status == 0 {
        success_empty()
    } else {
        internal_error(inner.message.as_deref().unwrap_or("Update failed"))
    }
}

pub async fn update_password(
    State(pool): State<MySqlPool>,
    Extension(auth_user): Extension<AuthUser>,
    Json(payload): Json<UpdatePasswordRequest>,
) -> (StatusCode, Json<ApiResponse<()>>) {
    let v1_resp =
        v1_update_password(State(pool.clone()), Extension(auth_user), Json(payload)).await;
    let inner = v1_resp.0;
    if inner.status == 0 {
        success_empty()
    } else if matches!(inner.status, 3 | 5) {
        internal_error(inner.message.as_deref().unwrap_or("Password update failed"))
    } else {
        bad_request(inner.message.as_deref().unwrap_or("Password update failed"))
    }
}

pub async fn regenerate_api_token(
    State(pool): State<MySqlPool>,
    Extension(auth_user): Extension<AuthUser>,
    headers: HeaderMap,
) -> (StatusCode, Json<ApiResponse<TokenData>>) {
    let raw_token = uuid::Uuid::new_v4().to_string();
    let token_hash = {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(raw_token.as_bytes());
        format!("{:x}", hasher.finalize())
    };

    // Create a new token entry in api_tokens table with all permissions
    match sqlx::query(
        "INSERT INTO api_tokens (user_id, name, token_hash, permissions) VALUES (?, ?, ?, ?)",
    )
    .bind(auth_user.user_id)
    .bind("Regenerated Token")
    .bind(&token_hash)
    .bind(crate::api::api_token::ALL_COMBINED as i64)
    .execute(&pool)
    .await
    {
        Ok(res) => {
            let username = username_for_log(&pool, auth_user.user_id).await;
            write_system_log(
                &pool,
                "info",
                "api_token",
                "api_token_created",
                &format!("{} regenerated API Token", username),
                Some(auth_user.user_id),
                Some(operation_metadata(
                    &headers,
                    "JWT",
                    serde_json::json!({
                        "token_id": res.last_insert_id(),
                        "name": "Regenerated Token",
                        "permissions": crate::api::api_token::ALL_COMBINED,
                        "username": username,
                    }),
                )),
            )
            .await;
            success(TokenData {
                api_token: raw_token,
            })
        }
        Err(e) => {
            log::error!("Failed to regenerate token: {:?}", e);
            internal_error("Failed to regenerate token")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn database_failure_does_not_report_success() {
        let pool = sqlx::mysql::MySqlPoolOptions::new()
            .connect_lazy("mysql://test:test@localhost/unused")
            .unwrap();
        pool.close().await;

        let (status, _) = update_info(
            State(pool.clone()),
            Extension(AuthUser { user_id: 1 }),
            Json(UpdateUserInfo {
                nickname: Some("test".into()),
                avatar: None,
            }),
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);

        let (status, _) = update_password(
            State(pool.clone()),
            Extension(AuthUser { user_id: 1 }),
            Json(UpdatePasswordRequest {
                old_password: Some("old".into()),
                new_password: Some("new".into()),
            }),
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);

        let response = crate::api::detail_list::get_detail_list(
            State(pool),
            Extension(AuthUser { user_id: 1 }),
        )
        .await;
        assert_ne!(response.0.status, 0);
        assert!(response.0.data.is_none());
    }
}
