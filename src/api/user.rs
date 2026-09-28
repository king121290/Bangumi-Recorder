use crate::auth_bearer::{AuthUser, hash_password, verify_password};
use axum::{
    Json,
    extract::{Extension, State},
};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sqlx::{Row, mysql::MySqlPool};

#[derive(Debug, Serialize)]
pub struct UserInfo {
    pub id: i64,
    pub uuid: String,
    pub username: String,
    pub nickname: String,
    pub email: String,
    pub avatar: String,
    pub status: i8,
    pub is_admin: bool,
    pub reg_time: Option<NaiveDate>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateUserInfo {
    pub nickname: Option<String>,
    pub avatar: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct UserResponse {
    pub status: i8,
    pub message: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdatePasswordRequest {
    pub old_password: Option<String>,
    pub new_password: Option<String>,
}

pub async fn get_info(
    State(pool): State<MySqlPool>,
    Extension(auth_user): Extension<AuthUser>,
) -> Json<UserInfo> {
    let user_id = auth_user.user_id;

    let user_info = sqlx::query(
        "SELECT id, uuid, username, nickname, email, avatar, status, is_admin, DATE(created_at) AS reg_time FROM users WHERE id = ?",
    )
    .bind(user_id)
    .fetch_one(&pool)
    .await
    .map(|row| UserInfo {
        id: row.try_get::<u32, _>("id").map(|id| id as i64).unwrap_or(0),
        uuid: row.try_get("uuid").unwrap_or_default(),
        username: row.try_get("username").unwrap_or_default(),
        nickname: row.try_get("nickname").unwrap_or_default(),
        email: row.try_get("email").unwrap_or_default(),
        avatar: row.try_get("avatar").unwrap_or_default(),
        status: row.try_get("status").unwrap_or(0),
        is_admin: row
            .try_get::<i8, _>("is_admin")
            .is_ok_and(|value| value != 0),
        reg_time: row.try_get("reg_time").ok(),
    });

    match user_info {
        Ok(info) => Json(info),
        Err(_) => Json(UserInfo {
            id: 0,
            uuid: String::new(),
            username: String::new(),
            nickname: String::new(),
            email: String::new(),
            avatar: String::new(),
            status: 0,
            is_admin: false,
            reg_time: None,
        }),
    }
}

pub async fn update_info(
    State(pool): State<MySqlPool>,
    Extension(auth_user): Extension<AuthUser>,
    Json(payload): Json<UpdateUserInfo>,
) -> Json<UserResponse> {
    let user_id = auth_user.user_id;

    match sqlx::query!(
        "UPDATE users SET nickname = COALESCE(?, nickname), avatar = COALESCE(?, avatar) WHERE id = ?",
        payload.nickname,
        payload.avatar,
        user_id
    )
    .execute(&pool)
    .await
    {
        Ok(_) => {}
        Err(e) => {
            log::error!("Failed to update user info for {}: {:?}", user_id, e);
            return Json(UserResponse {
                status: 5,
                message: Some("Database error".to_string()),
            });
        }
    };

    Json(UserResponse {
        status: 0,
        message: None,
    })
}

pub async fn update_password(
    State(pool): State<MySqlPool>,
    Extension(auth_user): Extension<AuthUser>,
    Json(payload): Json<UpdatePasswordRequest>,
) -> Json<UserResponse> {
    if payload.old_password.is_none() || payload.new_password.is_none() {
        return Json(UserResponse {
            status: 1,
            message: Some("Old password and new password are required".to_string()),
        });
    }

    let user_id = auth_user.user_id;
    let old_password = payload.old_password.clone().unwrap();
    let new_password = payload.new_password.clone().unwrap();

    let user_password_hash = sqlx::query!("SELECT password_hash FROM users WHERE id = ?", user_id)
        .fetch_optional(&pool)
        .await;

    match user_password_hash {
        Ok(Some(record)) => {
            if verify_password(&old_password, &record.password_hash) {
                let new_password_hash = match hash_password(new_password) {
                    Ok(hash) => hash,
                    Err(_) => {
                        return Json(UserResponse {
                            status: 3,
                            message: Some("Failed to hash new password".to_string()),
                        });
                    }
                };

                match sqlx::query!(
                    "UPDATE users SET password_hash = ? WHERE id = ?",
                    new_password_hash,
                    user_id
                )
                .execute(&pool)
                .await
                {
                    Ok(_) => {}
                    Err(e) => {
                        log::error!("Failed to update password for {}: {:?}", user_id, e);
                        return Json(UserResponse {
                            status: 5,
                            message: Some("Database error".to_string()),
                        });
                    }
                }

                Json(UserResponse {
                    status: 0,
                    message: Some("Password updated successfully".to_string()),
                })
            } else {
                Json(UserResponse {
                    status: 2,
                    message: Some("Old password is incorrect".to_string()),
                })
            }
        }
        Ok(None) => Json(UserResponse {
            status: 4,
            message: Some("User not found".to_string()),
        }),
        Err(_) => Json(UserResponse {
            status: 5,
            message: Some("Database error".to_string()),
        }),
    }
}
