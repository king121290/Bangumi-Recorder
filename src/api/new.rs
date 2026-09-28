use axum::{
    Json,
    extract::{Extension, State},
};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::mysql::MySqlPool;

use crate::api::imdb::{IMDB_SOURCE, ensure_imdb_item_cached, normalize_imdb_id};
use crate::api::logs::{LogTarget, write_recording_log};
use crate::api::search::{IDSearchQuery, search_bangumi_by_id};
use crate::auth_bearer::AuthUser;

#[derive(Debug, Deserialize, Serialize)]
pub struct AddRecordResponse {
    pub status: i32,
    pub source: Option<String>,
    pub external_id: Option<String>,
    pub imdb_id: Option<String>,
    pub local_external_media_id: Option<u32>,
    pub local_bangumi_id: Option<u32>,
    pub other_id: Option<u32>,
    pub bangumi_id: Option<u32>,
    pub recorder: Option<String>,
    pub date: Option<NaiveDate>,
}

#[derive(Debug, Deserialize)]
pub struct AddRecordQuery {
    pub bangumi_id: Option<u32>,
    pub source: Option<String>,
    pub external_id: Option<String>,
    pub imdb_id: Option<String>,
    pub use_api: Option<bool>,
    pub other_id: Option<u32>,
    pub other_title: Option<String>,
    pub other_description: Option<String>,
    pub other_cover: Option<String>,
    pub other_max_number: Option<i32>,
    pub other_status: Option<i32>,
    pub user_status: Option<i32>,
    pub recorder: Option<String>,
}

enum RecordTarget {
    Bangumi(u32),
    Imdb(String),
    Other,
}

fn empty_response(status: i32) -> AddRecordResponse {
    AddRecordResponse {
        status,
        source: None,
        external_id: None,
        imdb_id: None,
        local_external_media_id: None,
        local_bangumi_id: None,
        other_id: None,
        bangumi_id: None,
        recorder: None,
        date: None,
    }
}

fn detect_target(params: &AddRecordQuery) -> Option<RecordTarget> {
    let mut targets = Vec::new();

    if let Some(id) = params.bangumi_id {
        targets.push(RecordTarget::Bangumi(id));
    }

    let source = params
        .source
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();

    if source == "bangumi"
        && params.bangumi_id.is_none()
        && let Some(id) = params
            .external_id
            .as_deref()
            .and_then(|id| id.trim().parse::<u32>().ok())
    {
        targets.push(RecordTarget::Bangumi(id));
    }

    let media_id = params
        .imdb_id
        .as_deref()
        .or(params.external_id.as_deref())
        .and_then(normalize_imdb_id);
    if (source == IMDB_SOURCE || params.imdb_id.is_some() || media_id.is_some())
        && let Some(id) = media_id
    {
        targets.push(RecordTarget::Imdb(id));
    }

    if params.other_id.is_some()
        || params
            .other_title
            .as_ref()
            .is_some_and(|v| !v.trim().is_empty())
    {
        targets.push(RecordTarget::Other);
    }

    if targets.len() == 1 {
        targets.pop()
    } else {
        None
    }
}

async fn ensure_bangumi_easy_id(pool: &MySqlPool, bangumi_external_id: u32) -> Result<u32, i32> {
    match sqlx::query!(
        "SELECT id FROM bangumi_info_easy WHERE external_id = ?",
        bangumi_external_id
    )
    .fetch_optional(pool)
    .await
    {
        Ok(Some(record)) => return Ok(record.id),
        Ok(None) => {}
        Err(e) => {
            log::error!("Failed to query bangumi_info_easy: {}", e);
            return Err(-1);
        }
    }

    let _ = search_bangumi_by_id(
        State(pool.clone()),
        Json(IDSearchQuery {
            id: Some(bangumi_external_id),
            force: false,
        }),
    )
    .await;

    match sqlx::query!(
        "SELECT id FROM bangumi_info_easy WHERE external_id = ?",
        bangumi_external_id
    )
    .fetch_optional(pool)
    .await
    {
        Ok(Some(record)) => Ok(record.id),
        Ok(None) => {
            log::error!(
                "Bangumi with external_id {} not found after search",
                bangumi_external_id
            );
            Err(-2)
        }
        Err(e) => {
            log::error!("Failed to query bangumi_info_easy after search: {}", e);
            Err(-1)
        }
    }
}

async fn ensure_imdb_easy_id(
    pool: &MySqlPool,
    imdb_id: &str,
    use_api: Option<bool>,
) -> Result<u32, i32> {
    let imdb_id = normalize_imdb_id(imdb_id).ok_or(-1)?;
    if let Err(e) = ensure_imdb_item_cached(pool, &imdb_id, false, use_api).await {
        log::error!("IMDb item {} not found: {}", imdb_id, e);
        return Err(-2);
    }

    match sqlx::query!(
        "SELECT id FROM external_media WHERE source = ? AND external_id = ?",
        IMDB_SOURCE,
        imdb_id.as_str()
    )
    .fetch_optional(pool)
    .await
    {
        Ok(Some(record)) => Ok(record.id),
        Ok(None) => Err(-2),
        Err(e) => {
            log::error!("Failed to query cached IMDb item {}: {}", imdb_id, e);
            Err(-1)
        }
    }
}

async fn save_media_record(
    pool: &MySqlPool,
    user_id: i64,
    easy_id: u32,
    status: i32,
    recorder: &str,
) -> Result<Option<String>, i32> {
    match sqlx::query!(
        "SELECT id, recorder, is_delete FROM recordings WHERE user_id = ? AND bangumi_id = ? LIMIT 1",
        user_id,
        easy_id
    )
    .fetch_optional(pool)
    .await
    {
        Ok(Some(row)) => {
            if row.is_delete == 0 {
                return Err(-3);
            }
            sqlx::query!(
                "UPDATE recordings SET is_delete = 0, status = ?, recorder = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
                status,
                recorder,
                row.id
            )
            .execute(pool)
            .await
            .map_err(|e| {
                log::error!("Failed to restore soft-deleted media recording {}: {:?}", row.id, e);
                -1
            })?;
            Ok(Some(row.recorder.unwrap_or_default()))
        }
        Ok(None) => {
            sqlx::query!(
                "INSERT INTO recordings (user_id, bangumi_id, status, recorder) VALUES (?, ?, ?, ?)",
                user_id,
                easy_id,
                status,
                recorder
            )
            .execute(pool)
            .await
            .map_err(|e| {
                if let sqlx::Error::Database(db_err) = &e
                    && db_err.constraint() == Some("uk_recordings_user_bangumi")
                {
                    return -3;
                }
                log::error!("Failed to add media record: {}", e);
                -1
            })?;
            Ok(None)
        }
        Err(e) => {
            log::error!("Failed to check existing media record: {}", e);
            Err(-1)
        }
    }
}

async fn save_external_record(
    pool: &MySqlPool,
    user_id: i64,
    media_id: u32,
    status: i32,
    recorder: &str,
) -> Result<Option<String>, i32> {
    match sqlx::query!(
        "SELECT id, recorder, is_delete FROM recordings WHERE user_id = ? AND external_media_id = ? LIMIT 1",
        user_id,
        media_id
    )
    .fetch_optional(pool)
    .await
    {
        Ok(Some(row)) => {
            if row.is_delete == 0 {
                return Err(-3);
            }
            sqlx::query!(
                "UPDATE recordings SET is_delete = 0, status = ?, recorder = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
                status,
                recorder,
                row.id
            )
            .execute(pool)
            .await
            .map_err(|e| {
                log::error!("Failed to restore soft-deleted external media recording {}: {:?}", row.id, e);
                -1
            })?;
            Ok(Some(row.recorder.unwrap_or_default()))
        }
        Ok(None) => {
            sqlx::query!(
                "INSERT INTO recordings (user_id, external_media_id, status, recorder) VALUES (?, ?, ?, ?)",
                user_id,
                media_id,
                status,
                recorder
            )
            .execute(pool)
            .await
            .map_err(|e| {
                if let sqlx::Error::Database(db_err) = &e
                    && db_err.constraint() == Some("uk_recordings_user_external_media")
                {
                    return -3;
                }
                log::error!("Failed to add external media record: {}", e);
                -1
            })?;
            Ok(None)
        }
        Err(e) => {
            log::error!("Failed to check existing external media record: {}", e);
            Err(-1)
        }
    }
}

async fn create_or_resolve_other(
    pool: &MySqlPool,
    user_id: i64,
    params: &AddRecordQuery,
) -> Result<u32, i32> {
    if let Some(id) = params.other_id {
        // Check before both insertion and restoration of an existing recording.
        return sqlx::query_scalar::<_, u32>(
            "SELECT id FROM other_recorders WHERE id = ? AND (add_user IS NULL OR add_user = ?)",
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| {
            log::error!("Failed to resolve custom item: {}", e);
            -1
        })?
        .ok_or(-2);
    }

    let title = params
        .other_title
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let insert_result = if title.is_some() {
        sqlx::query!(
            "INSERT INTO other_recorders (name, description, cover_url, max_number, status, add_user) VALUES (?, ?, ?, ?, ?, ?)",
            title,
            params.other_description.as_deref(),
            params.other_cover.as_deref(),
            params.other_max_number,
            params.other_status,
            user_id
        )
        .execute(pool)
        .await
    } else {
        sqlx::query!("INSERT INTO other_recorders (add_user) VALUES (?)", user_id)
            .execute(pool)
            .await
    };

    insert_result
        .map(|result| result.last_insert_id() as u32)
        .map_err(|e| {
            log::error!("Failed to add other record: {}", e);
            -1
        })
}

async fn save_other_record(
    pool: &MySqlPool,
    user_id: i64,
    other_id: u32,
    status: i32,
    recorder: &str,
) -> Result<(Option<u32>, Option<String>), i32> {
    match sqlx::query!(
        "SELECT id, recorder, is_delete FROM recordings WHERE user_id = ? AND other_id = ? LIMIT 1",
        user_id,
        other_id
    )
    .fetch_optional(pool)
    .await
    {
        Ok(Some(row)) => {
            if row.is_delete == 0 {
                return Err(-3);
            }
            sqlx::query!(
                "UPDATE recordings SET is_delete = 0, status = ?, recorder = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
                status,
                recorder,
                row.id
            )
            .execute(pool)
            .await
            .map_err(|e| {
                log::error!("Failed to restore soft-deleted other recording {}: {:?}", row.id, e);
                -1
            })?;
            Ok((Some(row.id), Some(row.recorder.unwrap_or_default())))
        }
        Ok(None) => {
            let result = sqlx::query!(
                "INSERT INTO recordings (user_id, other_id, status, recorder) VALUES (?, ?, ?, ?)",
                user_id,
                other_id,
                status,
                recorder
            )
            .execute(pool)
            .await
            .map_err(|e| {
                if let sqlx::Error::Database(db_err) = &e
                    && db_err.constraint() == Some("uk_recordings_user_other")
                {
                    return -3;
                }
                log::error!("Failed to add recording for other record: {}", e);
                -1
            })?;
            Ok((Some(result.last_insert_id() as u32), None))
        }
        Err(e) => {
            log::error!("Failed to check existing other record: {}", e);
            Err(-1)
        }
    }
}

pub async fn add_record(
    State(pool): State<MySqlPool>,
    Extension(auth_user): Extension<AuthUser>,
    Json(params): Json<AddRecordQuery>,
) -> Json<AddRecordResponse> {
    let target = match detect_target(&params) {
        Some(target) => target,
        None => return Json(empty_response(-1)),
    };

    let user_status = params.user_status.unwrap_or(0);
    let recorder = params.recorder.clone().unwrap_or_default();
    let today = chrono::Utc::now().naive_utc().date();

    match target {
        RecordTarget::Bangumi(bangumi_external_id) => {
            let easy_id = match ensure_bangumi_easy_id(&pool, bangumi_external_id).await {
                Ok(id) => id,
                Err(status) => {
                    let mut response = empty_response(status);
                    response.source = Some("bangumi".to_string());
                    response.external_id = Some(bangumi_external_id.to_string());
                    response.bangumi_id = Some(bangumi_external_id);
                    return Json(response);
                }
            };

            match save_media_record(&pool, auth_user.user_id, easy_id, user_status, &recorder).await
            {
                Ok(_) => {
                    if let Ok(Some(recording_id)) = sqlx::query_scalar!(
                        "SELECT id FROM recordings WHERE user_id = ? AND bangumi_id = ? LIMIT 1",
                        auth_user.user_id,
                        easy_id
                    )
                    .fetch_optional(&pool)
                    .await
                    {
                        write_recording_log(
                            &pool,
                            recording_id,
                            Some(auth_user.user_id),
                            LogTarget::Bangumi(easy_id),
                            "recording_created",
                            None,
                            None,
                            None,
                            Some(json!({ "recorder": recorder, "status": user_status })),
                        )
                        .await;
                    }
                    Json(AddRecordResponse {
                        status: 0,
                        source: Some("bangumi".to_string()),
                        external_id: Some(bangumi_external_id.to_string()),
                        imdb_id: None,
                        local_external_media_id: None,
                        local_bangumi_id: Some(easy_id),
                        other_id: None,
                        bangumi_id: Some(bangumi_external_id),
                        recorder: Some(recorder),
                        date: Some(today),
                    })
                }
                Err(-3) => Json(AddRecordResponse {
                    status: -3,
                    source: Some("bangumi".to_string()),
                    external_id: Some(bangumi_external_id.to_string()),
                    imdb_id: None,
                    local_external_media_id: None,
                    local_bangumi_id: Some(easy_id),
                    other_id: None,
                    bangumi_id: Some(bangumi_external_id),
                    recorder: Some(recorder),
                    date: None,
                }),
                Err(status) => Json(empty_response(status)),
            }
        }
        RecordTarget::Imdb(imdb_id) => {
            let media_id = match ensure_imdb_easy_id(&pool, &imdb_id, params.use_api).await {
                Ok(id) => id,
                Err(status) => {
                    let mut response = empty_response(status);
                    response.source = Some(IMDB_SOURCE.to_string());
                    response.external_id = Some(imdb_id.clone());
                    response.imdb_id = Some(imdb_id);
                    return Json(response);
                }
            };

            match save_external_record(&pool, auth_user.user_id, media_id, user_status, &recorder)
                .await
            {
                Ok(_) => {
                    if let Ok(Some(recording_id)) = sqlx::query_scalar!(
                        "SELECT id FROM recordings WHERE user_id = ? AND external_media_id = ? LIMIT 1",
                        auth_user.user_id,
                        media_id
                    )
                    .fetch_optional(&pool)
                    .await
                    {
                        write_recording_log(
                            &pool,
                            recording_id,
                            Some(auth_user.user_id),
                            LogTarget::Imdb(media_id),
                            "recording_created",
                            None,
                            None,
                            None,
                            Some(json!({ "recorder": recorder, "status": user_status })),
                        )
                        .await;
                    }
                    Json(AddRecordResponse {
                        status: 0,
                        source: Some(IMDB_SOURCE.to_string()),
                        external_id: Some(imdb_id.clone()),
                        imdb_id: Some(imdb_id),
                        local_external_media_id: Some(media_id),
                        local_bangumi_id: None,
                        other_id: None,
                        bangumi_id: None,
                        recorder: Some(recorder),
                        date: Some(today),
                    })
                }
                Err(-3) => Json(AddRecordResponse {
                    status: -3,
                    source: Some(IMDB_SOURCE.to_string()),
                    external_id: Some(imdb_id.clone()),
                    imdb_id: Some(imdb_id),
                    local_external_media_id: Some(media_id),
                    local_bangumi_id: None,
                    other_id: None,
                    bangumi_id: None,
                    recorder: Some(recorder),
                    date: None,
                }),
                Err(status) => Json(empty_response(status)),
            }
        }
        RecordTarget::Other => {
            let other_id = match create_or_resolve_other(&pool, auth_user.user_id, &params).await {
                Ok(id) => id,
                Err(status) => return Json(empty_response(status)),
            };

            match save_other_record(&pool, auth_user.user_id, other_id, user_status, &recorder)
                .await
            {
                Ok((recording_id, _)) => {
                    if let Some(recording_id) = recording_id {
                        write_recording_log(
                            &pool,
                            recording_id,
                            Some(auth_user.user_id),
                            LogTarget::Other(other_id),
                            "recording_created",
                            None,
                            None,
                            None,
                            Some(json!({ "recorder": recorder, "status": user_status })),
                        )
                        .await;
                    }
                    Json(AddRecordResponse {
                        status: 0,
                        source: Some("custom".to_string()),
                        external_id: None,
                        imdb_id: None,
                        local_external_media_id: None,
                        local_bangumi_id: None,
                        other_id: Some(other_id),
                        bangumi_id: None,
                        recorder: Some(recorder),
                        date: Some(today),
                    })
                }
                Err(-3) => Json(AddRecordResponse {
                    status: -3,
                    source: Some("custom".to_string()),
                    external_id: None,
                    imdb_id: None,
                    local_external_media_id: None,
                    local_bangumi_id: None,
                    other_id: Some(other_id),
                    bangumi_id: None,
                    recorder: Some(recorder),
                    date: None,
                }),
                Err(status) => Json(empty_response(status)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{
        api_token::{PERM_READ, hash_token},
        v2,
    };
    use axum::{
        extract::{Path, Query},
        http::{HeaderMap, StatusCode},
    };

    #[tokio::test]
    #[ignore = "requires OTHER_ACCESS_TEST_DATABASE_URL pointing to a disposable MySQL database"]
    async fn other_access_is_scoped_to_authenticated_user() {
        // One connection keeps all fixtures in session-local temporary tables.
        // Never read DATABASE_URL or run the application's migrations/startup.
        let url =
            std::env::var("OTHER_ACCESS_TEST_DATABASE_URL").expect("explicit test database URL");
        let pool = sqlx::mysql::MySqlPoolOptions::new()
            .max_connections(1)
            .idle_timeout(None)
            .max_lifetime(None)
            .connect(&url)
            .await
            .unwrap();
        sqlx::query("CREATE TEMPORARY TABLE other_recorders (id INT UNSIGNED PRIMARY KEY, name VARCHAR(255), description TEXT, cover_url VARCHAR(255), max_number INT, status TINYINT, add_user INT UNSIGNED)")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO other_recorders (id, name, add_user) VALUES (1, 'public', NULL), (2, 'alice', 10), (3, 'bob', 20)")
            .execute(&pool).await.unwrap();
        sqlx::query("CREATE TEMPORARY TABLE api_tokens (user_id INT UNSIGNED, token_hash VARCHAR(64), permissions BIGINT UNSIGNED, is_active TINYINT, last_used_at DATETIME)")
            .execute(&pool).await.unwrap();
        for (user_id, token) in [(10_i64, "alice-token"), (20, "bob-token")] {
            sqlx::query("INSERT INTO api_tokens (user_id, token_hash, permissions, is_active) VALUES (?, ?, ?, 1)")
                .bind(user_id).bind(hash_token(token)).bind(PERM_READ)
                .execute(&pool).await.unwrap();
        }

        for (user_id, token) in [(10_i64, "alice-token"), (20, "bob-token")] {
            for id in 1..=4 {
                let visible = id == 1 || (id == 2 && user_id == 10) || (id == 3 && user_id == 20);
                // Spoofed JSON identity must not affect the authenticated caller.
                let params: AddRecordQuery = serde_json::from_value(json!({
                    "other_id": id, "user_id": 30 - user_id, "add_user": 30 - user_id,
                    "other_title": "must not create a fallback"
                }))
                .unwrap();
                assert_eq!(
                    create_or_resolve_other(&pool, user_id, &params).await,
                    if visible { Ok(id) } else { Err(-2) }
                );
                if !visible {
                    // Denial happens before any recordings lookup, restore, or insert.
                    let response = add_record(
                        State(pool.clone()),
                        Extension(AuthUser { user_id }),
                        Json(params),
                    )
                    .await;
                    assert_eq!(response.0.status, -2);
                    assert!(response.0.other_id.is_none());
                }
                let response = crate::api::search::get_other_by_id(
                    State(pool.clone()),
                    Extension(AuthUser { user_id }),
                    Json(IDSearchQuery {
                        id: Some(id),
                        force: false,
                    }),
                )
                .await;
                assert_eq!(response.0.status, if visible { 0 } else { -2 });
                assert_eq!(response.0.data.is_some(), visible);
                let (status, response) = v2::search::get_other(
                    State(pool.clone()),
                    Extension(AuthUser { user_id }),
                    Path(id),
                )
                .await;
                assert_eq!(
                    status,
                    if visible {
                        StatusCode::OK
                    } else {
                        StatusCode::NOT_FOUND
                    }
                );
                assert_eq!(response.0.data.is_some(), visible);

                // The header token wins over a conflicting query token.
                let mut headers = HeaderMap::new();
                headers.insert("authorization", format!("Bearer {token}").parse().unwrap());
                let (status, response) = v2::open::search::get_other(
                    State(pool.clone()),
                    Path(id),
                    headers,
                    Query(v2::open::search::BangumiQuery {
                        force: None,
                        token: Some(
                            if user_id == 10 {
                                "bob-token"
                            } else {
                                "alice-token"
                            }
                            .into(),
                        ),
                    }),
                )
                .await;
                assert_eq!(
                    status,
                    if visible {
                        StatusCode::OK
                    } else {
                        StatusCode::NOT_FOUND
                    }
                );
                assert_eq!(response.0.data.is_some(), visible);
            }
        }
        pool.close().await;
    }
}
