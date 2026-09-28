use std::collections::{BTreeMap, BTreeSet};

use axum::{
    Json,
    extract::{Extension, State},
    http::StatusCode,
};
use chrono::{NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{MySql, MySqlPool, Row, Transaction};

use super::episodes::{ensure_episode_metadata_cached, episode_has_user_visible_change};
use super::response::{ApiResponse, bad_request, internal_error, success};
use crate::api::logs::{LogTarget, write_recording_log};
use crate::api::search::{IDSearchQuery, search_bangumi_by_id};
use crate::auth_bearer::AuthUser;

const DEFAULT_PAGE_SIZE: usize = 500;
const MAX_PAGE_SIZE: usize = 2_000;
const MAX_RECORDS_PER_REQUEST: usize = 10_000;

struct PendingRecordingLog {
    recording_id: u32,
    bangumi_easy_id: u32,
    action: &'static str,
    field_name: Option<&'static str>,
    old_value: Option<Value>,
    new_value: Option<Value>,
    metadata: Value,
}

#[derive(Debug, Deserialize)]
pub struct SyncAniEpisodeInput {
    pub ordinal: i32,
    pub watched: bool,
    pub progress_seconds: Option<i32>,
    pub duration_seconds: Option<i32>,
    pub completed_at: Option<NaiveDateTime>,
    pub updated_at: NaiveDateTime,
}

#[derive(Debug, Deserialize)]
pub struct SyncAniRecordInput {
    pub bangumi_id: u32,
    pub recorder: Option<String>,
    pub user_status: i8,
    #[serde(default)]
    pub is_delete: bool,
    pub updated_at: NaiveDateTime,
    #[serde(default)]
    pub episodes: Vec<SyncAniEpisodeInput>,
}

#[derive(Debug, Deserialize)]
pub struct SyncAniRequest {
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub records: Vec<SyncAniRecordInput>,
}

#[derive(Debug, Serialize)]
pub struct SyncAniEpisode {
    pub ordinal: i32,
    pub watched: bool,
    pub progress_seconds: Option<i32>,
    pub duration_seconds: Option<i32>,
    pub completed_at: Option<NaiveDateTime>,
    pub updated_at: NaiveDateTime,
    pub revision: String,
}

#[derive(Debug, Serialize)]
pub struct SyncAniRecord {
    pub bangumi_id: u32,
    pub recorder: Option<String>,
    pub user_status: Option<i8>,
    pub is_delete: bool,
    pub updated_at: NaiveDateTime,
    pub revision: String,
    pub episodes: Vec<SyncAniEpisode>,
}

#[derive(Debug, Serialize)]
pub struct SyncAniResponse {
    pub records: Vec<SyncAniRecord>,
    pub server_time: NaiveDateTime,
    pub next_cursor: String,
    pub has_more: bool,
}

#[derive(Debug)]
enum SyncAniError {
    Invalid(String),
    Database(sqlx::Error),
}

fn tombstone_blocks_insert(
    latest_tombstone_at: Option<NaiveDateTime>,
    input_updated_at: NaiveDateTime,
) -> bool {
    latest_tombstone_at.is_some_and(|deleted_at| input_updated_at <= deleted_at)
}

impl From<sqlx::Error> for SyncAniError {
    fn from(value: sqlx::Error) -> Self {
        Self::Database(value)
    }
}

pub async fn sync_records(
    State(pool): State<MySqlPool>,
    Extension(auth_user): Extension<AuthUser>,
    Json(body): Json<SyncAniRequest>,
) -> (StatusCode, Json<ApiResponse<SyncAniResponse>>) {
    do_sync_records(&pool, auth_user.user_id, body).await
}

pub async fn do_sync_records(
    pool: &MySqlPool,
    user_id: i64,
    body: SyncAniRequest,
) -> (StatusCode, Json<ApiResponse<SyncAniResponse>>) {
    match sync(pool, user_id, body).await {
        Ok(data) => success(data),
        Err(SyncAniError::Invalid(message)) => bad_request(&message),
        Err(SyncAniError::Database(error)) => {
            log::error!("BR sync_ani database error for user {}: {}", user_id, error);
            internal_error("Sync failed")
        }
    }
}

async fn sync(
    pool: &MySqlPool,
    user_id: i64,
    body: SyncAniRequest,
) -> Result<SyncAniResponse, SyncAniError> {
    if body.records.len() > MAX_RECORDS_PER_REQUEST {
        return Err(SyncAniError::Invalid(format!(
            "At most {MAX_RECORDS_PER_REQUEST} records can be synchronized at once",
        )));
    }
    let cursor = body
        .cursor
        .as_deref()
        .unwrap_or("0")
        .parse::<u64>()
        .map_err(|_| SyncAniError::Invalid("Invalid sync cursor".to_string()))?;
    let limit = body
        .limit
        .unwrap_or(DEFAULT_PAGE_SIZE)
        .clamp(1, MAX_PAGE_SIZE);

    for record in &body.records {
        if !(0..=4).contains(&record.user_status) {
            return Err(SyncAniError::Invalid(format!(
                "Invalid user_status for Bangumi {}",
                record.bangumi_id
            )));
        }
        if record.episodes.iter().any(|episode| episode.ordinal <= 0) {
            return Err(SyncAniError::Invalid(format!(
                "Episode ordinals must be positive for Bangumi {}",
                record.bangumi_id
            )));
        }
    }

    let subject_ids = body
        .records
        .iter()
        .map(|record| record.bangumi_id)
        .collect::<BTreeSet<_>>();
    for bangumi_id in subject_ids {
        ensure_bangumi_in_pool(pool, bangumi_id).await?;
    }
    validate_episode_ordinals(pool, &body.records).await?;

    let mut tx = pool.begin().await?;
    let mut response_subjects = BTreeSet::new();
    let mut pending_logs = Vec::new();
    for record in &body.records {
        response_subjects.insert(record.bangumi_id);
        apply_record(&mut tx, user_id, record, &mut pending_logs).await?;
    }

    let change_rows = sqlx::query(
        "SELECT id, bangumi_id FROM sync_changes WHERE user_id = ? AND id > ? ORDER BY id ASC LIMIT ?",
    )
    .bind(user_id)
    .bind(cursor)
    .bind((limit + 1) as u64)
    .fetch_all(&mut *tx)
    .await?;
    let has_more = change_rows.len() > limit;
    let page = &change_rows[..change_rows.len().min(limit)];
    let next_cursor = page
        .last()
        .and_then(|row| row.try_get::<u64, _>("id").ok())
        .unwrap_or(cursor);
    for row in page {
        if let Ok(id) = row.try_get::<String, _>("bangumi_id")
            && let Ok(id) = id.parse::<u32>()
        {
            response_subjects.insert(id);
        }
    }
    tx.commit().await?;

    for entry in pending_logs {
        write_recording_log(
            pool,
            entry.recording_id,
            Some(user_id),
            LogTarget::Bangumi(entry.bangumi_easy_id),
            entry.action,
            entry.field_name,
            entry.old_value,
            entry.new_value,
            Some(entry.metadata),
        )
        .await;
    }

    let mut records = Vec::with_capacity(response_subjects.len());
    for bangumi_id in response_subjects {
        if let Some(record) = load_record(pool, user_id, bangumi_id).await? {
            records.push(record);
        }
    }

    Ok(SyncAniResponse {
        records,
        server_time: Utc::now().naive_utc(),
        next_cursor: next_cursor.to_string(),
        has_more,
    })
}

async fn ensure_bangumi_in_pool(pool: &MySqlPool, bangumi_id: u32) -> Result<u32, SyncAniError> {
    let external_id = bangumi_id.to_string();
    if let Some(easy_id) =
        sqlx::query_scalar::<_, u32>("SELECT id FROM bangumi_info_easy WHERE external_id = ?")
            .bind(&external_id)
            .fetch_optional(pool)
            .await?
    {
        return Ok(easy_id);
    }

    let lookup = search_bangumi_by_id(
        State(pool.clone()),
        Json(IDSearchQuery {
            id: Some(bangumi_id),
            force: false,
        }),
    )
    .await
    .0;

    if lookup.status != 0 {
        return Err(SyncAniError::Invalid(format!(
            "Bangumi {bangumi_id} does not exist or could not be fetched"
        )));
    }

    Ok(
        sqlx::query_scalar::<_, u32>("SELECT id FROM bangumi_info_easy WHERE external_id = ?")
            .bind(external_id)
            .fetch_one(pool)
            .await?,
    )
}

async fn cached_episode_ordinals(
    pool: &MySqlPool,
    easy_id: u32,
) -> Result<BTreeSet<i32>, sqlx::Error> {
    Ok(sqlx::query_scalar::<_, i32>(
        "SELECT ordinal FROM bangumi_episodes WHERE bangumi_easy_id = ?",
    )
    .bind(easy_id)
    .fetch_all(pool)
    .await?
    .into_iter()
    .collect())
}

async fn validate_episode_ordinals(
    pool: &MySqlPool,
    records: &[SyncAniRecordInput],
) -> Result<(), SyncAniError> {
    let mut requested = BTreeMap::<u32, BTreeSet<i32>>::new();
    for record in records
        .iter()
        .filter(|record| !record.is_delete && !record.episodes.is_empty())
    {
        requested
            .entry(record.bangumi_id)
            .or_default()
            .extend(record.episodes.iter().map(|episode| episode.ordinal));
    }

    for (bangumi_id, requested_ordinals) in requested {
        let easy_id = ensure_bangumi_in_pool(pool, bangumi_id).await?;
        let mut known_ordinals = cached_episode_ordinals(pool, easy_id).await?;
        if known_ordinals.is_empty() {
            if let Err(error) =
                ensure_episode_metadata_cached(pool, easy_id, &bangumi_id.to_string(), false).await
            {
                log::warn!(
                    "Unable to populate episode metadata before Animeko sync for Bangumi {}: {}",
                    bangumi_id,
                    error
                );
            }
            known_ordinals = cached_episode_ordinals(pool, easy_id).await?;
        }

        let missing = requested_ordinals
            .difference(&known_ordinals)
            .copied()
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(SyncAniError::Invalid(format!(
                "Episode ordinal(s) {} do not exist in cached metadata for Bangumi {}",
                missing
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", "),
                bangumi_id
            )));
        }
    }

    Ok(())
}

async fn ensure_bangumi(
    tx: &mut Transaction<'_, MySql>,
    bangumi_id: u32,
) -> Result<u32, sqlx::Error> {
    sqlx::query_scalar::<_, u32>("SELECT id FROM bangumi_info_easy WHERE external_id = ?")
        .bind(bangumi_id.to_string())
        .fetch_one(&mut **tx)
        .await
}

async fn apply_record(
    tx: &mut Transaction<'_, MySql>,
    user_id: i64,
    input: &SyncAniRecordInput,
    pending_logs: &mut Vec<PendingRecordingLog>,
) -> Result<(), sqlx::Error> {
    let easy_id = ensure_bangumi(tx, input.bangumi_id).await?;
    let existing = sqlx::query(
        "SELECT id, recorder, status, is_delete, updated_at FROM recordings \
         WHERE user_id = ? AND bangumi_id = ? LIMIT 1",
    )
    .bind(user_id)
    .bind(easy_id)
    .fetch_optional(&mut **tx)
    .await?;

    let (recording_id, is_delete) = if let Some(row) = existing {
        let id: u32 = row.try_get("id")?;
        let server_updated_at: NaiveDateTime = row.try_get("updated_at")?;
        let old_recorder = row.try_get::<Option<String>, _>("recorder")?;
        let old_status = row.try_get::<i8, _>("status")?;
        let old_is_delete = row.try_get::<i8, _>("is_delete")? != 0;
        let differs = old_recorder != input.recorder
            || old_status != input.user_status
            || old_is_delete != input.is_delete;
        // Equal timestamps deliberately keep the server state, preventing ping-pong.
        if differs && input.updated_at > server_updated_at {
            sqlx::query(
                "UPDATE recordings SET recorder = ?, status = ?, is_delete = ?, updated_at = ? WHERE id = ?",
            )
            .bind(&input.recorder)
            .bind(input.user_status)
            .bind(input.is_delete)
            .bind(input.updated_at)
            .bind(id)
            .execute(&mut **tx)
            .await?;
            if old_recorder != input.recorder {
                pending_logs.push(PendingRecordingLog {
                    recording_id: id,
                    bangumi_easy_id: easy_id,
                    action: "recorder_changed",
                    field_name: Some("recorder"),
                    old_value: old_recorder.as_ref().map(|value| json!(value)),
                    new_value: input.recorder.as_ref().map(|value| json!(value)),
                    metadata: json!({
                        "source": "animeko",
                        "bangumi_id": input.bangumi_id,
                        "client_updated_at": input.updated_at,
                    }),
                });
            }
            if old_status != input.user_status {
                pending_logs.push(PendingRecordingLog {
                    recording_id: id,
                    bangumi_easy_id: easy_id,
                    action: "status_changed",
                    field_name: Some("status"),
                    old_value: Some(json!(old_status)),
                    new_value: Some(json!(input.user_status)),
                    metadata: json!({
                        "source": "animeko",
                        "bangumi_id": input.bangumi_id,
                        "client_updated_at": input.updated_at,
                    }),
                });
            }
            if old_is_delete != input.is_delete {
                pending_logs.push(PendingRecordingLog {
                    recording_id: id,
                    bangumi_easy_id: easy_id,
                    action: if input.is_delete {
                        "recording_deleted"
                    } else {
                        "recording_restored"
                    },
                    field_name: Some("is_delete"),
                    old_value: Some(json!(old_is_delete)),
                    new_value: Some(json!(input.is_delete)),
                    metadata: json!({
                        "source": "animeko",
                        "bangumi_id": input.bangumi_id,
                        "client_updated_at": input.updated_at,
                    }),
                });
            }
            (id, input.is_delete)
        } else {
            (id, old_is_delete)
        }
    } else {
        // A recording may be absent because automatic cleanup physically removed an old soft-delete.
        // Keep the permanent change-stream tombstone authoritative so an offline client cannot
        // resurrect that deletion with stale state. A strictly newer update is an intentional restore.
        let latest_tombstone_at = sqlx::query_scalar::<_, NaiveDateTime>(
            "SELECT changed_at FROM sync_changes \
             WHERE user_id = ? AND bangumi_id = ? AND entity_type = 'record' AND is_delete = 1 \
             ORDER BY id DESC LIMIT 1",
        )
        .bind(user_id)
        .bind(input.bangumi_id.to_string())
        .fetch_optional(&mut **tx)
        .await?;
        if tombstone_blocks_insert(latest_tombstone_at, input.updated_at) {
            return Ok(());
        }

        let result = sqlx::query(
            "INSERT INTO recordings \
             (user_id, bangumi_id, recorder, status, is_delete, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(user_id)
        .bind(easy_id)
        .bind(&input.recorder)
        .bind(input.user_status)
        .bind(input.is_delete)
        .bind(input.updated_at)
        .bind(input.updated_at)
        .execute(&mut **tx)
        .await?;
        let id = result.last_insert_id() as u32;
        pending_logs.push(PendingRecordingLog {
            recording_id: id,
            bangumi_easy_id: easy_id,
            action: if input.is_delete {
                "recording_deleted"
            } else if latest_tombstone_at.is_some() {
                "recording_restored"
            } else {
                "recording_created"
            },
            field_name: None,
            old_value: latest_tombstone_at.map(|deleted_at| {
                json!({
                    "is_delete": true,
                    "updated_at": deleted_at,
                    "physical_tombstone": true,
                })
            }),
            new_value: Some(json!({
                "recorder": input.recorder,
                "user_status": input.user_status,
                "is_delete": input.is_delete,
                "updated_at": input.updated_at,
            })),
            metadata: json!({
                "source": "animeko",
                "bangumi_id": input.bangumi_id,
                "client_updated_at": input.updated_at,
            }),
        });
        (id, input.is_delete)
    };

    if !is_delete {
        for episode in &input.episodes {
            apply_episode(
                tx,
                recording_id,
                easy_id,
                input.bangumi_id,
                episode,
                pending_logs,
            )
            .await?;
        }
    }
    Ok(())
}

async fn apply_episode(
    tx: &mut Transaction<'_, MySql>,
    recording_id: u32,
    bangumi_easy_id: u32,
    bangumi_id: u32,
    input: &SyncAniEpisodeInput,
    pending_logs: &mut Vec<PendingRecordingLog>,
) -> Result<(), sqlx::Error> {
    let existing = sqlx::query(
        "SELECT watched, progress_seconds, duration_seconds, completed_at, updated_at \
         FROM episode_records WHERE recording_id = ? AND ordinal = ? LIMIT 1",
    )
    .bind(recording_id)
    .bind(input.ordinal)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(row) = existing {
        let server_updated_at: NaiveDateTime = row.try_get("updated_at")?;
        let old_watched = row.try_get::<i8, _>("watched")? != 0;
        let old_progress_seconds = row.try_get::<Option<i32>, _>("progress_seconds")?;
        let old_duration_seconds = row.try_get::<Option<i32>, _>("duration_seconds")?;
        let old_completed_at = row.try_get::<Option<NaiveDateTime>, _>("completed_at")?;
        let has_user_visible_change = episode_update_has_user_visible_change(
            old_watched,
            old_progress_seconds,
            old_duration_seconds,
            input,
        );
        let differs = old_watched != input.watched
            || old_progress_seconds != input.progress_seconds
            || old_duration_seconds != input.duration_seconds
            || old_completed_at != input.completed_at;
        if differs && input.updated_at > server_updated_at {
            sqlx::query(
                "UPDATE episode_records SET watched = ?, progress_seconds = ?, duration_seconds = ?, \
                 completed_at = ?, updated_at = ? WHERE recording_id = ? AND ordinal = ?",
            )
            .bind(input.watched)
            .bind(input.progress_seconds)
            .bind(input.duration_seconds)
            .bind(input.completed_at)
            .bind(input.updated_at)
            .bind(recording_id)
            .bind(input.ordinal)
            .execute(&mut **tx)
            .await?;
            if has_user_visible_change {
                pending_logs.push(PendingRecordingLog {
                    recording_id,
                    bangumi_easy_id,
                    action: "episode_updated",
                    field_name: None,
                    old_value: Some(json!({
                        "ordinal": input.ordinal,
                        "watched": old_watched,
                        "progress_seconds": old_progress_seconds,
                        "duration_seconds": old_duration_seconds,
                        "completed_at": old_completed_at,
                        "updated_at": server_updated_at,
                    })),
                    new_value: Some(json!({
                        "ordinal": input.ordinal,
                        "watched": input.watched,
                        "progress_seconds": input.progress_seconds,
                        "duration_seconds": input.duration_seconds,
                        "completed_at": input.completed_at,
                        "updated_at": input.updated_at,
                    })),
                    metadata: json!({
                        "source": "animeko",
                        "bangumi_id": bangumi_id,
                        "ordinal": input.ordinal,
                        "client_updated_at": input.updated_at,
                    }),
                });
            }
        }
    } else {
        sqlx::query(
            "INSERT INTO episode_records \
             (recording_id, ordinal, watched, progress_seconds, duration_seconds, completed_at, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(recording_id)
        .bind(input.ordinal)
        .bind(input.watched)
        .bind(input.progress_seconds)
        .bind(input.duration_seconds)
        .bind(input.completed_at)
        .bind(input.updated_at)
        .bind(input.updated_at)
        .execute(&mut **tx)
        .await?;
        if episode_has_user_visible_change(false, None, input.watched, input.progress_seconds) {
            pending_logs.push(PendingRecordingLog {
                recording_id,
                bangumi_easy_id,
                action: "episode_created",
                field_name: None,
                old_value: None,
                new_value: Some(json!({
                    "ordinal": input.ordinal,
                    "watched": input.watched,
                    "progress_seconds": input.progress_seconds,
                    "duration_seconds": input.duration_seconds,
                    "completed_at": input.completed_at,
                    "updated_at": input.updated_at,
                })),
                metadata: json!({
                    "source": "animeko",
                    "bangumi_id": bangumi_id,
                    "ordinal": input.ordinal,
                    "client_updated_at": input.updated_at,
                }),
            });
        }
    }
    Ok(())
}

fn episode_update_has_user_visible_change(
    old_watched: bool,
    old_progress_seconds: Option<i32>,
    _old_duration_seconds: Option<i32>,
    input: &SyncAniEpisodeInput,
) -> bool {
    episode_has_user_visible_change(
        old_watched,
        old_progress_seconds,
        input.watched,
        input.progress_seconds,
    )
}

async fn load_record(
    pool: &MySqlPool,
    user_id: i64,
    bangumi_id: u32,
) -> Result<Option<SyncAniRecord>, sqlx::Error> {
    let external_id = bangumi_id.to_string();
    let row = sqlx::query(
        "SELECT r.id, r.recorder, r.status, r.is_delete, r.updated_at, \
         CAST(COALESCE((SELECT MAX(sc.id) FROM sync_changes sc WHERE sc.user_id = r.user_id \
         AND sc.bangumi_id = b.external_id AND sc.entity_type = 'record'), 0) AS UNSIGNED) AS revision \
         FROM recordings r JOIN bangumi_info_easy b ON b.id = r.bangumi_id \
         WHERE r.user_id = ? AND b.external_id = ? LIMIT 1",
    )
    .bind(user_id)
    .bind(&external_id)
    .fetch_optional(pool)
    .await?;

    let Some(row) = row else {
        let tombstone = sqlx::query(
            "SELECT id, changed_at FROM sync_changes \
             WHERE user_id = ? AND bangumi_id = ? AND entity_type = 'record' AND is_delete = 1 \
             ORDER BY id DESC LIMIT 1",
        )
        .bind(user_id)
        .bind(external_id)
        .fetch_optional(pool)
        .await?;
        return tombstone
            .map(|row| {
                Ok(SyncAniRecord {
                    bangumi_id,
                    recorder: None,
                    user_status: None,
                    is_delete: true,
                    updated_at: row.try_get("changed_at")?,
                    revision: row.try_get::<u64, _>("id")?.to_string(),
                    episodes: Vec::new(),
                })
            })
            .transpose();
    };

    let recording_id: u32 = row.try_get("id")?;
    let is_delete = row.try_get::<i8, _>("is_delete")? != 0;
    let episodes = if is_delete {
        Vec::new()
    } else {
        sqlx::query(
            "SELECT e.ordinal, e.watched, e.progress_seconds, e.duration_seconds, e.completed_at, e.updated_at, \
             CAST(COALESCE((SELECT MAX(sc.id) FROM sync_changes sc WHERE sc.user_id = ? \
             AND sc.bangumi_id = ? AND sc.entity_type = 'episode' AND sc.ordinal = e.ordinal), 0) AS UNSIGNED) AS revision \
             FROM episode_records e WHERE e.recording_id = ? ORDER BY e.ordinal ASC",
        )
        .bind(user_id)
        .bind(&external_id)
        .bind(recording_id)
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(|episode| {
            Ok(SyncAniEpisode {
                ordinal: episode.try_get("ordinal")?,
                watched: episode.try_get::<i8, _>("watched")? != 0,
                progress_seconds: episode.try_get("progress_seconds")?,
                duration_seconds: episode.try_get("duration_seconds")?,
                completed_at: episode.try_get("completed_at")?,
                updated_at: episode.try_get("updated_at")?,
                revision: episode.try_get::<u64, _>("revision")?.to_string(),
            })
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()?
    };

    Ok(Some(SyncAniRecord {
        bangumi_id,
        recorder: row.try_get("recorder")?,
        user_status: Some(row.try_get("status")?),
        is_delete,
        updated_at: row.try_get("updated_at")?,
        revision: row.try_get::<u64, _>("revision")?.to_string(),
        episodes,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn time(value: &str) -> NaiveDateTime {
        value.parse().unwrap()
    }

    fn record(
        bangumi_id: u32,
        recorder: &str,
        status: i8,
        is_delete: bool,
        updated_at: NaiveDateTime,
    ) -> SyncAniRecordInput {
        SyncAniRecordInput {
            bangumi_id,
            recorder: Some(recorder.to_string()),
            user_status: status,
            is_delete,
            updated_at,
            episodes: if is_delete {
                Vec::new()
            } else {
                vec![
                    SyncAniEpisodeInput {
                        ordinal: 1,
                        watched: true,
                        progress_seconds: Some(1_440),
                        duration_seconds: Some(1_440),
                        completed_at: Some(updated_at),
                        updated_at,
                    },
                    SyncAniEpisodeInput {
                        ordinal: 2,
                        watched: false,
                        progress_seconds: Some(300),
                        duration_seconds: Some(1_440),
                        completed_at: None,
                        updated_at,
                    },
                ]
            },
        }
    }

    #[test]
    fn cursor_is_a_string_to_avoid_json_precision_loss() {
        let request: SyncAniRequest =
            serde_json::from_str(r#"{"cursor":"18446744073709551615","records":[]}"#).unwrap();
        assert_eq!(request.cursor.as_deref(), Some("18446744073709551615"));
    }

    #[test]
    fn unknown_fields_do_not_break_old_or_new_clients() {
        let request: SyncAniRequest =
            serde_json::from_str(r#"{"records":[],"future_capability":true}"#).unwrap();
        assert!(request.records.is_empty());
    }

    #[test]
    fn zero_and_missing_progress_do_not_create_user_visible_episode_logs() {
        let updated_at = time("2026-07-16T05:02:23.325");
        let mut input = SyncAniEpisodeInput {
            ordinal: 1,
            watched: false,
            progress_seconds: Some(0),
            duration_seconds: None,
            completed_at: None,
            updated_at,
        };

        assert!(!episode_update_has_user_visible_change(
            false, None, None, &input,
        ));

        input.progress_seconds = None;
        assert!(!episode_update_has_user_visible_change(
            false,
            Some(0),
            None,
            &input,
        ));

        input.progress_seconds = Some(1);
        assert!(episode_update_has_user_visible_change(
            false,
            Some(0),
            None,
            &input,
        ));
    }

    #[test]
    fn duration_only_refresh_does_not_create_user_visible_episode_logs() {
        let input = SyncAniEpisodeInput {
            ordinal: 1,
            watched: false,
            progress_seconds: Some(0),
            duration_seconds: Some(1_440),
            completed_at: None,
            updated_at: time("2026-07-16T05:02:23.325"),
        };

        assert!(!episode_has_user_visible_change(
            false,
            None,
            input.watched,
            input.progress_seconds,
        ));
        assert!(!episode_update_has_user_visible_change(
            false,
            Some(0),
            Some(0),
            &input,
        ));
        assert!(episode_update_has_user_visible_change(
            false,
            Some(30),
            Some(0),
            &input,
        ));
    }

    #[test]
    fn completion_timestamp_backfill_does_not_create_user_visible_episode_logs() {
        let updated_at = time("2026-07-16T05:02:23.325");
        let input = SyncAniEpisodeInput {
            ordinal: 1,
            watched: true,
            progress_seconds: Some(1_202),
            duration_seconds: Some(1_439),
            completed_at: Some(time("2026-07-13T09:02:19.043")),
            updated_at,
        };

        assert!(!episode_update_has_user_visible_change(
            true,
            Some(1_202),
            Some(1_439),
            &input,
        ));
        assert!(episode_update_has_user_visible_change(
            false,
            Some(1_202),
            Some(1_439),
            &input,
        ));
    }

    #[test]
    fn permanent_tombstone_rejects_stale_and_exact_tie_resurrection() {
        let deleted_at = "2026-07-12T12:00:00".parse::<NaiveDateTime>().unwrap();
        let older = "2026-07-12T11:59:59".parse::<NaiveDateTime>().unwrap();

        assert!(tombstone_blocks_insert(Some(deleted_at), older));
        assert!(tombstone_blocks_insert(Some(deleted_at), deleted_at));
    }

    #[test]
    fn permanent_tombstone_allows_only_a_strictly_newer_restore() {
        let deleted_at = "2026-07-12T12:00:00".parse::<NaiveDateTime>().unwrap();
        let newer = "2026-07-12T12:00:00.000001"
            .parse::<NaiveDateTime>()
            .unwrap();

        assert!(!tombstone_blocks_insert(Some(deleted_at), newer));
        assert!(!tombstone_blocks_insert(None, deleted_at));
    }

    /// Opt-in stateful test. The database must be disposable because this runs all migrations.
    #[tokio::test]
    async fn live_mariadb_sync_is_bidirectional_idempotent_and_cleanup_safe() {
        let Ok(database_url) = std::env::var("BR_SYNC_ANI_TEST_DATABASE_URL") else {
            return;
        };
        let pool = MySqlPool::connect(&database_url).await.unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();

        let unique = uuid::Uuid::new_v4();
        let username = format!("sync-ani-test-{unique}");
        let user_id = sqlx::query(
            "INSERT INTO users (username, password_hash, api_token_hash, uuid) VALUES (?, 'unused', ?, ?)",
        )
        .bind(&username)
        .bind(format!("{:064x}", unique.as_u128()))
        .bind(unique.to_string())
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_id() as i64;
        let bangumi_id = 900_000_000 + (unique.as_u128() % 90_000_000) as u32;
        let easy_id = sqlx::query(
            "INSERT INTO bangumi_info_easy (external_id, title, type) VALUES (?, 'sync-ani test', 1)",
        )
        .bind(bangumi_id.to_string())
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_id() as u32;
        for ordinal in [1, 2] {
            sqlx::query(
                "INSERT INTO bangumi_episodes \
                 (bangumi_easy_id, ordinal, ep_label, title, name_cn, fetch_generation) \
                 VALUES (?, ?, ?, ?, ?, 'sync-ani-live-test')",
            )
            .bind(easy_id)
            .bind(ordinal)
            .bind(ordinal.to_string())
            .bind(format!("Episode {ordinal}"))
            .bind(format!("Episode {ordinal}"))
            .execute(&pool)
            .await
            .unwrap();
        }

        let result = async {
            let t1 = time("2024-01-01T01:00:00.000001");
            let t2 = time("2024-01-01T02:00:00.000002");
            let t3 = time("2024-01-01T03:00:00.000003");
            let t4 = time("2024-01-01T04:00:00.000004");
            let t5 = time("2024-01-01T05:00:00.000005");
            let t6 = time("2024-01-01T06:00:00.000006");
            let t7 = time("2024-01-01T07:00:00.000007");

            let mut invalid = record(bangumi_id, "invalid", 1, false, t1);
            invalid.episodes[0].ordinal = 3;
            let invalid_result = sync(
                &pool,
                user_id,
                SyncAniRequest {
                    cursor: Some("0".to_string()),
                    limit: None,
                    records: vec![invalid],
                },
            )
            .await;
            assert!(matches!(invalid_result, Err(SyncAniError::Invalid(_))));
            let recording_count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM recordings WHERE user_id = ? AND bangumi_id = ?",
            )
            .bind(user_id)
            .bind(easy_id)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(recording_count, 0);

            let initial = sync(
                &pool,
                user_id,
                SyncAniRequest {
                    cursor: Some("0".to_string()),
                    limit: Some(1),
                    records: vec![record(bangumi_id, "1|05:00", 1, false, t1)],
                },
            )
            .await
            .unwrap();
            let initial_record = initial
                .records
                .iter()
                .find(|record| record.bangumi_id == bangumi_id)
                .unwrap();
            assert_eq!(initial_record.user_status, Some(1));
            assert_eq!(initial_record.episodes.len(), 2);
            assert!(initial_record.episodes[0].watched);
            assert_eq!(initial_record.episodes[1].progress_seconds, Some(300));

            let changes_after_initial: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM sync_changes WHERE user_id = ? AND bangumi_id = ?",
            )
            .bind(user_id)
            .bind(bangumi_id.to_string())
            .fetch_one(&pool)
            .await
            .unwrap();
            sync(
                &pool,
                user_id,
                SyncAniRequest {
                    cursor: Some(initial.next_cursor),
                    limit: None,
                    records: vec![record(bangumi_id, "1|05:00", 1, false, t1)],
                },
            )
            .await
            .unwrap();
            let changes_after_replay: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM sync_changes WHERE user_id = ? AND bangumi_id = ?",
            )
            .bind(user_id)
            .bind(bangumi_id.to_string())
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(changes_after_replay, changes_after_initial);

            let exact_tie = sync(
                &pool,
                user_id,
                SyncAniRequest {
                    cursor: Some("0".to_string()),
                    limit: None,
                    records: vec![record(bangumi_id, "tie-loses", 4, false, t1)],
                },
            )
            .await
            .unwrap();
            assert_eq!(exact_tie.records[0].user_status, Some(1));
            assert_eq!(exact_tie.records[0].recorder.as_deref(), Some("1|05:00"));

            let newer = sync(
                &pool,
                user_id,
                SyncAniRequest {
                    cursor: Some("0".to_string()),
                    limit: None,
                    records: vec![record(bangumi_id, "newer", 2, false, t2)],
                },
            )
            .await
            .unwrap();
            assert_eq!(newer.records[0].user_status, Some(2));

            sqlx::query(
                "UPDATE recordings SET recorder = 'remote-newer', status = 3, updated_at = ? \
                 WHERE user_id = ? AND bangumi_id = ?",
            )
            .bind(t3)
            .bind(user_id)
            .bind(easy_id)
            .execute(&pool)
            .await
            .unwrap();
            let older_client = sync(
                &pool,
                user_id,
                SyncAniRequest {
                    cursor: Some("0".to_string()),
                    limit: None,
                    records: vec![record(bangumi_id, "older-client", 4, false, t2)],
                },
            )
            .await
            .unwrap();
            assert_eq!(older_client.records[0].user_status, Some(3));
            assert_eq!(
                older_client.records[0].recorder.as_deref(),
                Some("remote-newer")
            );

            let deleted = sync(
                &pool,
                user_id,
                SyncAniRequest {
                    cursor: Some("0".to_string()),
                    limit: None,
                    records: vec![record(bangumi_id, "deleted", 3, true, t4)],
                },
            )
            .await
            .unwrap();
            assert!(deleted.records[0].is_delete);
            let stale_restore = sync(
                &pool,
                user_id,
                SyncAniRequest {
                    cursor: Some("0".to_string()),
                    limit: None,
                    records: vec![record(bangumi_id, "stale", 1, false, t3)],
                },
            )
            .await
            .unwrap();
            assert!(stale_restore.records[0].is_delete);

            let restored = sync(
                &pool,
                user_id,
                SyncAniRequest {
                    cursor: Some("0".to_string()),
                    limit: None,
                    records: vec![record(bangumi_id, "restored", 1, false, t5)],
                },
            )
            .await
            .unwrap();
            assert!(!restored.records[0].is_delete);

            sync(
                &pool,
                user_id,
                SyncAniRequest {
                    cursor: Some("0".to_string()),
                    limit: None,
                    records: vec![record(bangumi_id, "deleted-again", 1, true, t6)],
                },
            )
            .await
            .unwrap();
            sqlx::query("DELETE FROM recordings WHERE user_id = ? AND bangumi_id = ?")
                .bind(user_id)
                .bind(easy_id)
                .execute(&pool)
                .await
                .unwrap();
            let permanent_deleted_at: NaiveDateTime = sqlx::query_scalar(
                "SELECT changed_at FROM sync_changes WHERE user_id = ? AND bangumi_id = ? \
                 AND entity_type = 'record' AND is_delete = 1 ORDER BY id DESC LIMIT 1",
            )
            .bind(user_id)
            .bind(bangumi_id.to_string())
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(permanent_deleted_at, t6);

            let blocked_after_cleanup = sync(
                &pool,
                user_id,
                SyncAniRequest {
                    cursor: Some("0".to_string()),
                    limit: None,
                    records: vec![record(bangumi_id, "still-stale", 1, false, t5)],
                },
            )
            .await
            .unwrap();
            assert!(blocked_after_cleanup.records[0].is_delete);
            assert_eq!(blocked_after_cleanup.records[0].updated_at, t6);

            let restored_after_cleanup = sync(
                &pool,
                user_id,
                SyncAniRequest {
                    cursor: Some("0".to_string()),
                    limit: None,
                    records: vec![record(bangumi_id, "restored-after-cleanup", 1, false, t7)],
                },
            )
            .await
            .unwrap();
            assert!(!restored_after_cleanup.records[0].is_delete);
            assert_eq!(restored_after_cleanup.records[0].updated_at, t7);

            let before_final_replay: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM sync_changes WHERE user_id = ? AND bangumi_id = ?",
            )
            .bind(user_id)
            .bind(bangumi_id.to_string())
            .fetch_one(&pool)
            .await
            .unwrap();
            sync(
                &pool,
                user_id,
                SyncAniRequest {
                    cursor: Some("0".to_string()),
                    limit: None,
                    records: vec![record(bangumi_id, "restored-after-cleanup", 1, false, t7)],
                },
            )
            .await
            .unwrap();
            let after_final_replay: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM sync_changes WHERE user_id = ? AND bangumi_id = ?",
            )
            .bind(user_id)
            .bind(bangumi_id.to_string())
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(after_final_replay, before_final_replay);

            let log_actions: Vec<String> = sqlx::query_scalar(
                "SELECT action FROM recording_logs WHERE user_id = ? ORDER BY id",
            )
            .bind(user_id)
            .fetch_all(&pool)
            .await
            .unwrap();
            assert!(log_actions.iter().any(|action| action == "episode_created"));
            assert!(
                log_actions
                    .iter()
                    .any(|action| action == "recording_deleted")
            );
            assert!(
                log_actions
                    .iter()
                    .any(|action| action == "recording_restored")
            );
            assert!(
                log_actions
                    .iter()
                    .any(|action| action == "recorder_changed")
            );
            assert!(log_actions.iter().any(|action| action == "status_changed"));
            assert!(
                log_actions
                    .iter()
                    .all(|action| !action.starts_with("sync_ani_"))
            );
        }
        .await;

        sqlx::query("DELETE FROM recording_logs WHERE user_id = ?")
            .bind(user_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM users WHERE id = ?")
            .bind(user_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM bangumi_info_easy WHERE external_id = ?")
            .bind(bangumi_id.to_string())
            .execute(&pool)
            .await
            .unwrap();

        result
    }
}
