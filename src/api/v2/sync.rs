use std::collections::HashMap;

use axum::{
    Json,
    extract::{Extension, Query, State},
    http::StatusCode,
};
use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::QueryBuilder;
use sqlx::mysql::MySqlPool;

use super::response::{ApiResponse, bad_request, internal_error, success};
use crate::api::logs::{LogTarget, write_recording_log};
use crate::api::search::{IDSearchQuery, search_bangumi_by_id};
use crate::auth_bearer::AuthUser;

#[derive(Debug, PartialEq, Eq)]
enum PendingSyncAction {
    Upsert {
        easy_id: u32,
        recorder: Option<String>,
        status: i8,
        updated_at: NaiveDateTime,
    },
    CreateById {
        bangumi_id: u32,
        recorder: Option<String>,
        status: i8,
        updated_at: NaiveDateTime,
    },
}

#[derive(Deserialize)]
pub struct SyncRequestRecord {
    pub bangumi_id: String,
    pub recorder: Option<String>,
    pub user_status: Option<i32>,
    pub updated_at: Option<NaiveDateTime>,
}

#[derive(Deserialize)]
pub struct SyncRequestBody {
    pub records: Vec<SyncRequestRecord>,
}

#[derive(Serialize, Clone)]
pub struct SyncResponseRecord {
    pub bangumi_id: String,
    pub recorder: Option<String>,
    pub user_status: Option<i8>,
    pub updated_at: NaiveDateTime,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_delete: Option<bool>,
}

#[derive(Serialize)]
pub struct SyncResponseData {
    pub records: Vec<SyncResponseRecord>,
    pub deleted: Vec<String>,
}

#[derive(Deserialize)]
pub struct SyncSinceQuery {
    pub since: Option<NaiveDateTime>,
}

#[derive(sqlx::FromRow)]
struct ExistingRecordingState {
    id: u32,
    recorder: Option<String>,
    status: i8,
    updated_at: NaiveDateTime,
    is_delete: i8,
}

// Current rows supersede historical tombstones, including explicit restores elsewhere.
const SYNC_STATE_SQL: &str = "SELECT b.external_id AS bangumi_id, r.recorder, \
    r.status AS user_status, r.updated_at, r.is_delete FROM recordings r \
    JOIN bangumi_info_easy b ON b.id = r.bangumi_id WHERE r.user_id = ? \
    UNION ALL SELECT sc.bangumi_id, NULL, NULL, sc.changed_at, sc.is_delete \
    FROM sync_changes sc WHERE sc.user_id = ? AND sc.entity_type = 'record' \
    AND sc.is_delete = 1 AND NOT EXISTS (SELECT 1 FROM sync_changes newer \
        WHERE newer.user_id = sc.user_id AND newer.bangumi_id = sc.bangumi_id \
        AND newer.entity_type = 'record' AND newer.is_delete = 1 AND newer.id > sc.id) \
    AND NOT EXISTS (SELECT 1 FROM recordings r JOIN bangumi_info_easy b ON b.id = r.bangumi_id \
        WHERE r.user_id = sc.user_id AND b.external_id = sc.bangumi_id)";

#[derive(sqlx::FromRow)]
struct SyncStateRow {
    bangumi_id: String,
    recorder: Option<String>,
    user_status: Option<i8>,
    updated_at: NaiveDateTime,
    is_delete: i8,
}

impl From<SyncStateRow> for SyncResponseRecord {
    fn from(row: SyncStateRow) -> Self {
        Self {
            bangumi_id: row.bangumi_id,
            recorder: if row.is_delete != 0 {
                None
            } else {
                row.recorder
            },
            user_status: if row.is_delete != 0 {
                None
            } else {
                row.user_status
            },
            updated_at: row.updated_at,
            is_delete: (row.is_delete != 0).then_some(true),
        }
    }
}

fn build_pending_sync_actions(
    records: &[SyncRequestRecord],
    external_to_easy: &HashMap<String, u32>,
) -> Vec<PendingSyncAction> {
    let mut actions = Vec::with_capacity(records.len());
    for client_rec in records {
        if let Some(&easy_id) = external_to_easy.get(&client_rec.bangumi_id) {
            let status = client_rec.user_status.map(|s| s as i8).unwrap_or(0);
            let client_ts = client_rec
                .updated_at
                .unwrap_or_else(|| chrono::Utc::now().naive_utc());
            actions.push(PendingSyncAction::Upsert {
                easy_id,
                recorder: client_rec.recorder.clone(),
                status,
                updated_at: client_ts,
            });
            continue;
        }

        if let Ok(bangumi_id) = client_rec.bangumi_id.parse::<u32>() {
            let status = client_rec.user_status.map(|s| s as i8).unwrap_or(0);
            let client_ts = client_rec
                .updated_at
                .unwrap_or_else(|| chrono::Utc::now().naive_utc());
            actions.push(PendingSyncAction::CreateById {
                bangumi_id,
                recorder: client_rec.recorder.clone(),
                status,
                updated_at: client_ts,
            });
        }
    }
    actions
}

async fn do_sync(
    pool: &MySqlPool,
    user_id: i64,
    body: SyncRequestBody,
) -> Result<SyncResponseData, ()> {
    const MAX_SYNC_RECORDS: usize = 10_000;
    if body.records.len() > MAX_SYNC_RECORDS {
        log::warn!(
            "User {} attempted to sync {} records (limit: {})",
            user_id,
            body.records.len(),
            MAX_SYNC_RECORDS
        );
        return Err(());
    }

    let mut external_to_easy: HashMap<String, u32> = HashMap::with_capacity(body.records.len());
    if !body.records.is_empty() {
        let mut qb = QueryBuilder::new(
            "SELECT id, external_id FROM bangumi_info_easy WHERE external_id IN (",
        );
        let mut sep = qb.separated(", ");
        for rec in &body.records {
            sep.push_bind(rec.bangumi_id.as_str());
        }
        sep.push_unseparated(")");
        let rows: Vec<(u32, String)> = qb
            .build_query_as()
            .fetch_all(pool)
            .await
            .map_err(|e| log::error!("batch resolve bangumi error: {:?}", e))?;
        for (id, external_id) in rows {
            external_to_easy.insert(external_id, id);
        }
    }

    // Step 2: Build pending actions for each record, including auto-creation for unresolved IDs
    let pending_actions = build_pending_sync_actions(&body.records, &external_to_easy);
    let mut to_write: Vec<(u32, Option<String>, i8, NaiveDateTime)> =
        Vec::with_capacity(pending_actions.len());
    for action in &pending_actions {
        match action {
            PendingSyncAction::Upsert {
                easy_id,
                recorder,
                status,
                updated_at,
            } => to_write.push((*easy_id, recorder.clone(), *status, *updated_at)),
            PendingSyncAction::CreateById {
                bangumi_id,
                recorder,
                status,
                updated_at,
            } => {
                // Resolve metadata only: add_record also writes/restores user state outside our transaction.
                let _ = search_bangumi_by_id(
                    State(pool.clone()),
                    Json(IDSearchQuery {
                        id: Some(*bangumi_id),
                        force: false,
                    }),
                )
                .await;

                if let Some(easy_id) = sqlx::query_scalar::<_, u32>(
                    "SELECT id FROM bangumi_info_easy WHERE external_id = ?",
                )
                .bind(bangumi_id.to_string())
                .fetch_optional(pool)
                .await
                .map_err(|e| log::error!("resolve bangumi error: {:?}", e))?
                {
                    let client_ts = *updated_at;
                    to_write.push((easy_id, recorder.clone(), *status, client_ts));
                }
            }
        }
    }

    // Step 3: Lock and arbitrate each write before assigning any fields.
    let mut tx = pool.begin().await.map_err(|e| {
        log::error!("tx begin error: {:?}", e);
    })?;

    let mut applied = Vec::new();
    let mut created = Vec::new();
    // Stable ordering reduces deadlocks between overlapping requests; retain duplicate input order.
    to_write.sort_by_key(|(easy_id, _, _, _)| *easy_id);
    for (easy_id, recorder, status, updated_at) in to_write {
        let old = sqlx::query_as::<_, ExistingRecordingState>(
            "SELECT id, recorder, status, updated_at, is_delete FROM recordings \
             WHERE user_id = ? AND bangumi_id = ? FOR UPDATE",
        )
        .bind(user_id)
        .bind(easy_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| log::error!("recording lock error: {:?}", e))?;
        if let Some(old) = old {
            if old.is_delete != 0
                || updated_at <= old.updated_at
                || (old.recorder == recorder && old.status == status)
            {
                continue;
            }
            sqlx::query(
                "UPDATE recordings SET recorder = ?, status = ?, updated_at = ? WHERE id = ?",
            )
            .bind(&recorder)
            .bind(status)
            .bind(updated_at)
            .bind(old.id)
            .execute(&mut *tx)
            .await
            .map_err(|e| log::error!("sync update error: {:?}", e))?;
            applied.push((easy_id, recorder, status, old));
        } else {
            // This protocol has no explicit restore intent. Missing timestamps must not revive deletions.
            let tombstone = sqlx::query_scalar::<_, u64>(
                "SELECT sc.id FROM sync_changes sc \
                 JOIN bangumi_info_easy b ON b.external_id = sc.bangumi_id \
                 WHERE sc.user_id = ? AND b.id = ? AND sc.entity_type = 'record' AND sc.is_delete = 1 \
                 ORDER BY sc.id DESC LIMIT 1 FOR UPDATE",
            )
            .bind(user_id).bind(easy_id).fetch_optional(&mut *tx).await
            .map_err(|e| log::error!("sync tombstone query error: {:?}", e))?;
            if tombstone.is_some() {
                continue;
            }
            let result = sqlx::query(
                "INSERT INTO recordings (user_id, bangumi_id, recorder, status, updated_at, created_at) \
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(user_id).bind(easy_id).bind(&recorder).bind(status).bind(updated_at).bind(updated_at)
            .execute(&mut *tx).await
            .map_err(|e| log::error!("sync insert error: {:?}", e))?;
            created.push((result.last_insert_id() as u32, easy_id, recorder, status));
        }
    }

    // Step 4: Query authoritative server state (post-write)
    let server_rows = sqlx::query_as::<_, SyncStateRow>(SYNC_STATE_SQL)
        .bind(user_id)
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| log::error!("server state query error: {:?}", e))?;

    tx.commit().await.map_err(|e| {
        log::error!("tx commit error: {:?}", e);
    })?;

    for (id, easy_id, recorder, status) in created {
        write_recording_log(
            pool,
            id,
            Some(user_id),
            LogTarget::Bangumi(easy_id),
            "recording_created",
            None,
            None,
            None,
            Some(json!({ "source": "sync", "recorder": recorder, "status": status })),
        )
        .await;
    }

    for (easy_id, recorder, status, old) in &applied {
        if old.recorder != *recorder {
            write_recording_log(
                pool,
                old.id,
                Some(user_id),
                LogTarget::Bangumi(*easy_id),
                "recorder_changed",
                Some("recorder"),
                old.recorder.as_ref().map(|v| json!(v)),
                recorder.as_ref().map(|v| json!(v)),
                Some(json!({ "source": "sync" })),
            )
            .await;
        }
        if old.status != *status {
            write_recording_log(
                pool,
                old.id,
                Some(user_id),
                LogTarget::Bangumi(*easy_id),
                "status_changed",
                Some("status"),
                Some(json!(old.status)),
                Some(json!(status)),
                Some(json!({ "source": "sync" })),
            )
            .await;
        }
    }

    let mut records: Vec<SyncResponseRecord> = Vec::with_capacity(server_rows.len());
    let mut deleted: Vec<String> = Vec::new();

    for r in server_rows {
        let bangumi_id = r.bangumi_id;
        if r.is_delete != 0 {
            deleted.push(bangumi_id);
        } else {
            records.push(SyncResponseRecord {
                bangumi_id,
                recorder: r.recorder,
                user_status: r.user_status,
                updated_at: r.updated_at,
                is_delete: None,
            });
        }
    }

    Ok(SyncResponseData { records, deleted })
}

async fn do_incremental_sync(
    pool: &MySqlPool,
    user_id: i64,
    since: NaiveDateTime,
) -> Result<Vec<SyncResponseRecord>, ()> {
    let sql = format!("SELECT * FROM ({SYNC_STATE_SQL}) state WHERE updated_at > ?");
    let rows = sqlx::query_as::<_, SyncStateRow>(&sql)
        .bind(user_id)
        .bind(user_id)
        .bind(since)
        .fetch_all(pool)
        .await;

    match rows {
        Ok(rows) => {
            let records: Vec<SyncResponseRecord> =
                rows.into_iter().map(SyncResponseRecord::from).collect();
            Ok(records)
        }
        Err(e) => {
            log::error!("DB error: {:?}", e);
            Err(())
        }
    }
}

pub async fn sync_records(
    State(pool): State<MySqlPool>,
    Extension(auth_user): Extension<AuthUser>,
    Json(body): Json<SyncRequestBody>,
) -> (StatusCode, Json<ApiResponse<SyncResponseData>>) {
    match do_sync(&pool, auth_user.user_id, body).await {
        Ok(data) => success(data),
        Err(_) => internal_error("Sync failed"),
    }
}

pub async fn incremental_sync(
    State(pool): State<MySqlPool>,
    Extension(auth_user): Extension<AuthUser>,
    Query(query): Query<SyncSinceQuery>,
) -> (StatusCode, Json<ApiResponse<Vec<SyncResponseRecord>>>) {
    let since = match query.since {
        Some(s) => s,
        None => return bad_request("Missing 'since' query parameter"),
    };

    match do_incremental_sync(&pool, auth_user.user_id, since).await {
        Ok(records) => success(records),
        Err(_) => internal_error("Database error"),
    }
}

pub async fn do_sync_records(
    pool: &MySqlPool,
    user_id: i64,
    body: SyncRequestBody,
) -> (StatusCode, Json<ApiResponse<SyncResponseData>>) {
    match do_sync(pool, user_id, body).await {
        Ok(data) => success(data),
        Err(_) => internal_error("Sync failed"),
    }
}

pub async fn do_incremental_sync_records(
    pool: &MySqlPool,
    user_id: i64,
    since: NaiveDateTime,
) -> (StatusCode, Json<ApiResponse<Vec<SyncResponseRecord>>>) {
    match do_incremental_sync(pool, user_id, since).await {
        Ok(records) => success(records),
        Err(_) => internal_error("Database error"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn time(value: &str) -> NaiveDateTime {
        value.parse().unwrap()
    }

    #[test]
    fn incremental_wire_format_keeps_array_and_live_record_fields() {
        let live = SyncResponseRecord::from(SyncStateRow {
            bangumi_id: "1001".into(),
            recorder: Some("1|00:00".into()),
            user_status: Some(2),
            updated_at: time("2026-01-01T00:00:00"),
            is_delete: 0,
        });
        assert_eq!(
            serde_json::to_value(vec![live]).unwrap(),
            json!([{
                "bangumi_id": "1001", "recorder": "1|00:00", "user_status": 2,
                "updated_at": "2026-01-01T00:00:00"
            }])
        );
    }

    #[test]
    fn incremental_deletion_is_additive_and_has_no_live_payload() {
        let deleted = SyncResponseRecord::from(SyncStateRow {
            bangumi_id: "1001".into(),
            recorder: Some("stale progress".into()),
            user_status: Some(2),
            updated_at: time("2026-01-01T00:00:00.000001"),
            is_delete: 1,
        });
        assert_eq!(
            serde_json::to_value(vec![deleted]).unwrap(),
            json!([{
                "bangumi_id": "1001", "recorder": null, "user_status": null,
                "updated_at": "2026-01-01T00:00:00.000001", "is_delete": true
            }])
        );
        let legacy: SyncRequestBody = serde_json::from_value(json!({
            "records": [{ "bangumi_id": "1001" }]
        }))
        .unwrap();
        assert!(legacy.records[0].updated_at.is_none());
    }

    // Only run against an explicitly provisioned disposable database, never DATABASE_URL or .env.
    #[tokio::test]
    #[ignore = "requires BR_SYNC_TEST_DATABASE_URL pointing to a disposable br_sync_test database"]
    async fn live_mysql_conflicts_deletions_and_tombstones() {
        let url = std::env::var("BR_SYNC_TEST_DATABASE_URL").expect("explicit test URL required");
        let options: sqlx::mysql::MySqlConnectOptions = url.parse().unwrap();
        assert_eq!(options.get_database(), Some("br_sync_test"));
        let pool = MySqlPool::connect_with(options).await.unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        let unique = uuid::Uuid::new_v4();
        let user_id = sqlx::query(
            "INSERT INTO users (username, password_hash, api_token_hash, uuid) VALUES (?, 'unused', ?, ?)",
        )
        .bind(format!("sync-test-{unique}"))
        .bind(format!("{:064x}", unique.as_u128()))
        .bind(unique.to_string()).execute(&pool).await.unwrap().last_insert_id() as i64;
        let external_id = format!("sync-test-{unique}");
        let easy_id = sqlx::query(
            "INSERT INTO bangumi_info_easy (external_id, title, type) VALUES (?, 'sync test', 8)",
        )
        .bind(&external_id)
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_id() as u32;
        let t1 = time("2024-01-01T01:00:00.000001");
        let t2 = time("2024-01-01T02:00:00.000002");
        let t3 = time("2024-01-01T03:00:00.000003");
        let t4 = time("2024-01-01T04:00:00.000004");
        let input = |recorder: Option<&str>, status: i32, updated_at| SyncRequestRecord {
            bangumi_id: external_id.clone(),
            recorder: recorder.map(str::to_owned),
            user_status: Some(status),
            updated_at,
        };
        let sync = |records| do_sync(&pool, user_id, SyncRequestBody { records });

        let initial = sync(vec![input(None, 0, Some(t1))]).await.unwrap();
        assert_eq!(initial.records[0].updated_at, t1);
        // Recorder-only and status-only changes both advance the timestamp.
        let changed = sync(vec![input(Some("new"), 0, Some(t2))]).await.unwrap();
        assert_eq!(changed.records[0].updated_at, t2);
        assert_eq!(changed.records[0].recorder.as_deref(), Some("new"));
        let changed = sync(vec![input(Some("new"), 2, Some(t3))]).await.unwrap();
        assert_eq!(changed.records[0].updated_at, t3);
        assert_eq!(changed.records[0].user_status, Some(2));
        for record in [
            input(Some("stale"), 4, Some(t2)),
            input(None, 4, Some(t3)),
            input(Some("new"), 2, Some(t4)),
        ] {
            let unchanged = sync(vec![record]).await.unwrap();
            assert_eq!(unchanged.records[0].updated_at, t3);
            assert_eq!(unchanged.records[0].recorder.as_deref(), Some("new"));
            assert_eq!(unchanged.records[0].user_status, Some(2));
        }
        assert!(
            do_incremental_sync(&pool, user_id, t3)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            do_incremental_sync(&pool, user_id, t2).await.unwrap().len(),
            1
        );
        // Repeated subjects arbitrate against each preceding accepted write, not a stale batch snapshot.
        let changed = sync(vec![
            input(None, 3, Some(t4)),
            input(Some("stale"), 1, Some(t2)),
        ])
        .await
        .unwrap();
        assert_eq!(changed.records[0].updated_at, t4);
        assert_eq!(changed.records[0].recorder, None);
        assert_eq!(changed.records[0].user_status, Some(3));

        sqlx::query("UPDATE recordings SET is_delete = 1, updated_at = ? WHERE user_id = ? AND bangumi_id = ?")
            .bind(t4).bind(user_id).bind(easy_id).execute(&pool).await.unwrap();
        let logs_before: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM recording_logs WHERE user_id = ?")
                .bind(user_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        for hard_delete in [false, true] {
            if hard_delete {
                sqlx::query("DELETE FROM recordings WHERE user_id = ? AND bangumi_id = ?")
                    .bind(user_id)
                    .bind(easy_id)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
            for records in [
                vec![],
                vec![input(Some("old"), 1, Some(t1))],
                vec![input(Some("tie"), 1, Some(t4))],
                vec![input(Some("undated"), 1, None)],
            ] {
                let deleted = sync(records).await.unwrap();
                assert!(deleted.records.is_empty());
                assert_eq!(deleted.deleted, vec![external_id.clone()]);
            }
            let delta = do_incremental_sync(&pool, user_id, t3).await.unwrap();
            assert_eq!(delta.len(), 1);
            assert_eq!(delta[0].is_delete, Some(true));
            assert_eq!(delta[0].updated_at, t4);
            assert!(
                do_incremental_sync(&pool, user_id, t4)
                    .await
                    .unwrap()
                    .is_empty()
            );
        }
        let logs_after: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM recording_logs WHERE user_id = ?")
                .bind(user_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(logs_before, logs_after);
        // An explicit restore through another API must suppress all historical deletion events.
        sqlx::query("INSERT INTO recordings (user_id, bangumi_id, recorder, status, updated_at) VALUES (?, ?, 'restored', 1, ?)")
            .bind(user_id).bind(easy_id).bind(t4).execute(&pool).await.unwrap();
        let restored = sync(vec![]).await.unwrap();
        assert!(restored.deleted.is_empty());
        assert_eq!(restored.records.len(), 1);
        let delta = do_incremental_sync(&pool, user_id, t3).await.unwrap();
        assert_eq!(delta.len(), 1);
        assert_eq!(delta[0].is_delete, None);
        assert!(do_incremental_sync(&pool, 0, t1).await.unwrap().is_empty());
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
        sqlx::query("DELETE FROM bangumi_info_easy WHERE id = ?")
            .bind(easy_id)
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }

    #[test]
    fn build_pending_sync_actions_creates_missing_numeric_ids() {
        let records = vec![
            SyncRequestRecord {
                bangumi_id: "1001".to_string(),
                recorder: Some("tv".to_string()),
                user_status: Some(2),
                updated_at: None,
            },
            SyncRequestRecord {
                bangumi_id: "2002".to_string(),
                recorder: None,
                user_status: None,
                updated_at: None,
            },
        ];
        let mut external_to_easy = HashMap::new();
        external_to_easy.insert("2002".to_string(), 42);

        let actions = build_pending_sync_actions(&records, &external_to_easy);

        assert_eq!(actions.len(), 2);
        assert!(matches!(
            actions[0],
            PendingSyncAction::CreateById {
                bangumi_id: 1001,
                ..
            }
        ));
        assert!(matches!(
            actions[1],
            PendingSyncAction::Upsert { easy_id: 42, .. }
        ));
    }
}
