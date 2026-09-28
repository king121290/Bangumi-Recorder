use axum::{
    Json,
    extract::{Extension, State},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::mysql::MySqlPool;

use crate::api::imdb::{IMDB_SOURCE, normalize_imdb_id};
use crate::api::logs::{LogTarget, write_recording_log, write_system_log};
use crate::auth_bearer::AuthUser;

#[derive(Deserialize)]
pub struct DeleteRecorderQuery {
    pub bangumi_id: Option<u32>,
    pub source: Option<String>,
    pub external_id: Option<String>,
    pub imdb_id: Option<String>,
    pub other_id: Option<u32>,
    pub hard_delete: Option<bool>,
}

#[derive(Serialize)]
pub struct DeleteRecorderResponse {
    pub status: i32,
    pub message: Option<String>,
}

fn response(status: i32, message: &str) -> Json<DeleteRecorderResponse> {
    Json(DeleteRecorderResponse {
        status,
        message: Some(message.to_string()),
    })
}

fn other_delete_sql(
    add_user: Option<u32>,
    user_id: i64,
    hard_delete: bool,
) -> (&'static str, bool) {
    let delete_original = add_user.is_some_and(|owner| i64::from(owner) == user_id);
    let sql = if delete_original {
        "DELETE FROM other_recorders WHERE id = ? AND add_user = ?"
    } else if hard_delete {
        "DELETE FROM recordings WHERE other_id = ? AND user_id = ? AND is_delete = 0"
    } else {
        "UPDATE recordings SET is_delete = 1 WHERE other_id = ? AND user_id = ? AND is_delete = 0"
    };
    (sql, delete_original)
}

pub async fn delete_recorder(
    State(pool): State<MySqlPool>,
    Extension(auth_user): Extension<AuthUser>,
    Json(params): Json<DeleteRecorderQuery>,
) -> Json<DeleteRecorderResponse> {
    let hard_delete = params.hard_delete.unwrap_or(false);

    if let Some(other_id) = params.other_id {
        let (recording_id, add_user) = match sqlx::query_as::<_, (u32, Option<u32>)>(
            r#"SELECT r.id, o.add_user
               FROM recordings r
               JOIN other_recorders o ON o.id = r.other_id
               WHERE r.user_id = ? AND r.other_id = ? AND r.is_delete = 0"#,
        )
        .bind(auth_user.user_id)
        .bind(other_id)
        .fetch_optional(&pool)
        .await
        {
            Ok(Some(row)) => row,
            Ok(None) => return response(-3, "Recording not found"),
            Err(e) => {
                log::error!("Failed to query custom recording before delete: {}", e);
                return response(-2, "Database error");
            }
        };

        let (sql, delete_original) = other_delete_sql(add_user, auth_user.user_id, hard_delete);
        let effective_hard_delete = delete_original || hard_delete;
        let result = sqlx::query(sql)
            .bind(other_id)
            .bind(auth_user.user_id)
            .execute(&pool)
            .await;
        let deleted = delete_by_sql_result(result, effective_hard_delete);
        if deleted.0.status != 0 {
            return deleted;
        }

        write_recording_log(
            &pool,
            recording_id,
            Some(auth_user.user_id),
            LogTarget::Other(other_id),
            if effective_hard_delete {
                "recording_hard_deleted"
            } else {
                "recording_deleted"
            },
            Some("is_delete"),
            Some(json!(0)),
            Some(json!(1)),
            Some(json!({ "also_delete_original_other_recording": delete_original })),
        )
        .await;

        write_system_log(
            &pool,
            "info",
            "recording",
            "other_recording_deleted",
            if delete_original {
                "Deleted custom recording and original custom item"
            } else if hard_delete {
                "Hard deleted custom recording"
            } else {
                "Soft deleted custom recording"
            },
            Some(auth_user.user_id),
            Some(json!({
                "recording_id": recording_id,
                "other_id": other_id,
                "hard_delete": effective_hard_delete,
                "also_delete_original_other_recording": delete_original,
            })),
        )
        .await;

        return deleted;
    }

    let normalized_imdb_id = params
        .imdb_id
        .as_deref()
        .or_else(|| {
            if params
                .source
                .as_deref()
                .unwrap_or_default()
                .eq_ignore_ascii_case(IMDB_SOURCE)
            {
                params.external_id.as_deref()
            } else {
                None
            }
        })
        .and_then(normalize_imdb_id);
    let target_count = [params.bangumi_id.is_some(), normalized_imdb_id.is_some()]
        .into_iter()
        .filter(|v| *v)
        .count();

    if target_count != 1 {
        return response(-1, "Missing media id");
    }

    if let Some(bangumi_id) = params.bangumi_id {
        let local_id = match sqlx::query!(
            "SELECT id FROM bangumi_info_easy WHERE external_id = ?",
            bangumi_id
        )
        .fetch_optional(&pool)
        .await
        {
            Ok(Some(record)) => record.id,
            Ok(None) => return response(-2, "Bangumi not found"),
            Err(e) => {
                log::error!("Failed to query bangumi_info_easy: {}", e);
                return response(-2, "Database error");
            }
        };

        let recording = match sqlx::query!(
            "SELECT id, is_delete FROM recordings WHERE user_id = ? AND bangumi_id = ? AND is_delete = 0",
            auth_user.user_id,
            local_id
        )
        .fetch_optional(&pool)
        .await
        {
            Ok(Some(row)) => row,
            Ok(None) => return response(-3, "Recording not found"),
            Err(e) => {
                log::error!("Failed to query recording before delete: {}", e);
                return response(-2, "Database error");
            }
        };
        write_recording_log(
            &pool,
            recording.id,
            Some(auth_user.user_id),
            LogTarget::Bangumi(local_id),
            if hard_delete {
                "recording_hard_deleted"
            } else {
                "recording_deleted"
            },
            Some("is_delete"),
            Some(json!(recording.is_delete)),
            Some(json!(1)),
            None,
        )
        .await;

        let result = if hard_delete {
            sqlx::query!(
                "DELETE FROM recordings WHERE user_id = ? AND bangumi_id = ?",
                auth_user.user_id,
                local_id
            )
            .execute(&pool)
            .await
        } else {
            sqlx::query!(
                "UPDATE recordings SET is_delete = 1 WHERE user_id = ? AND bangumi_id = ? AND is_delete = 0",
                auth_user.user_id,
                local_id
            )
            .execute(&pool)
            .await
        };
        return delete_by_sql_result(result, hard_delete);
    }

    let imdb_id = match normalized_imdb_id {
        Some(id) => id,
        None => return response(-1, "Invalid IMDb id"),
    };

    let local_id = match sqlx::query!(
        "SELECT id FROM external_media WHERE source = ? AND external_id = ?",
        IMDB_SOURCE,
        imdb_id
    )
    .fetch_optional(&pool)
    .await
    {
        Ok(Some(record)) => record.id,
        Ok(None) => return response(-2, "IMDb title not found"),
        Err(e) => {
            log::error!("Failed to query external_media: {}", e);
            return response(-2, "Database error");
        }
    };

    let recording = match sqlx::query!(
        "SELECT id, is_delete FROM recordings WHERE user_id = ? AND external_media_id = ? AND is_delete = 0",
        auth_user.user_id,
        local_id
    )
    .fetch_optional(&pool)
    .await
    {
        Ok(Some(row)) => row,
        Ok(None) => return response(-3, "Recording not found"),
        Err(e) => {
            log::error!("Failed to query recording before delete: {}", e);
            return response(-2, "Database error");
        }
    };
    write_recording_log(
        &pool,
        recording.id,
        Some(auth_user.user_id),
        LogTarget::Imdb(local_id),
        if hard_delete {
            "recording_hard_deleted"
        } else {
            "recording_deleted"
        },
        Some("is_delete"),
        Some(json!(recording.is_delete)),
        Some(json!(1)),
        None,
    )
    .await;

    let result = if hard_delete {
        sqlx::query!(
            "DELETE FROM recordings WHERE user_id = ? AND external_media_id = ?",
            auth_user.user_id,
            local_id
        )
        .execute(&pool)
        .await
    } else {
        sqlx::query!(
            "UPDATE recordings SET is_delete = 1 WHERE user_id = ? AND external_media_id = ? AND is_delete = 0",
            auth_user.user_id,
            local_id
        )
        .execute(&pool)
        .await
    };
    delete_by_sql_result(result, hard_delete)
}

fn delete_by_sql_result(
    result: Result<sqlx::mysql::MySqlQueryResult, sqlx::Error>,
    hard_delete: bool,
) -> Json<DeleteRecorderResponse> {
    match result {
        Ok(result) => {
            if result.rows_affected() == 0 {
                response(-3, "Recording not found")
            } else {
                response(
                    0,
                    if hard_delete {
                        "Hard deleted successfully"
                    } else {
                        "Deleted successfully"
                    },
                )
            }
        }
        Err(e) => {
            log::error!("Failed to delete recording: {}", e);
            response(-2, "Failed to delete recording")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_owner_can_delete_original_custom_item() {
        for hard_delete in [false, true] {
            assert_eq!(
                other_delete_sql(Some(10), 10, hard_delete),
                (
                    "DELETE FROM other_recorders WHERE id = ? AND add_user = ?",
                    true
                )
            );
            // Public items and historical links to another user's private item
            // must both remain intact, regardless of the requested delete mode.
            for add_user in [None, Some(20)] {
                assert_eq!(
                    other_delete_sql(add_user, 10, hard_delete),
                    (
                        if hard_delete {
                            "DELETE FROM recordings WHERE other_id = ? AND user_id = ? AND is_delete = 0"
                        } else {
                            "UPDATE recordings SET is_delete = 1 WHERE other_id = ? AND user_id = ? AND is_delete = 0"
                        },
                        false
                    )
                );
            }
        }
    }

    #[test]
    fn unsuccessful_delete_is_not_reported_as_success() {
        for hard_delete in [false, true] {
            let missing = delete_by_sql_result(Ok(Default::default()), hard_delete);
            assert_eq!(missing.0.status, -3);
            let failed = delete_by_sql_result(Err(sqlx::Error::RowNotFound), hard_delete);
            assert_eq!(failed.0.status, -2);
        }
    }
}
