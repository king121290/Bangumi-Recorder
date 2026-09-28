use axum::{
    Json,
    extract::{Extension, Path, Query, State},
    http::StatusCode,
};
use chrono::{NaiveDate, NaiveDateTime, Utc};
use reqwest::Client;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{QueryBuilder, Row, mysql::MySqlPool};
use std::{collections::HashSet, sync::OnceLock};

use super::response::{ApiResponse, internal_error, not_found, success, success_with_message};
use crate::api::logs::{LogTarget, write_recording_log, write_system_log};
use crate::auth_bearer::AuthUser;

fn http_client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .user_agent("Mozilla/5.0")
            .build()
            .unwrap_or_else(|_| Client::new())
    })
}

fn selector(pattern: &'static str, slot: &'static OnceLock<Selector>) -> &'static Selector {
    slot.get_or_init(|| Selector::parse(pattern).expect("valid CSS selector"))
}

#[derive(Serialize, Debug)]
pub struct EpisodeItem {
    pub ordinal: i32,
    pub label: Option<String>,
    pub title: Option<String>,
    pub name_cn: Option<String>,
    pub airdate: Option<NaiveDate>,
    pub duration: Option<String>,
    pub watched: bool,
    pub progress_seconds: Option<i32>,
    pub duration_seconds: Option<i32>,
    pub completed_at: Option<NaiveDateTime>,
    pub updated_at: Option<NaiveDateTime>,
}

#[derive(Deserialize)]
pub struct UpdateEpisodeBody {
    pub watched: Option<bool>,
    pub progress_seconds: Option<i32>,
    pub duration_seconds: Option<i32>,
}

pub(super) fn episode_has_user_visible_change(
    old_watched: bool,
    old_progress_seconds: Option<i32>,
    new_watched: bool,
    new_progress_seconds: Option<i32>,
) -> bool {
    old_watched != new_watched
        || old_progress_seconds.filter(|seconds| *seconds != 0)
            != new_progress_seconds.filter(|seconds| *seconds != 0)
}

#[derive(Deserialize)]
pub struct ForceEpisodesQuery {
    pub force: Option<bool>,
}

pub async fn list_episodes(
    State(pool): State<MySqlPool>,
    Extension(auth_user): Extension<AuthUser>,
    Path(bangumi_id): Path<u32>,
    Query(query): Query<ForceEpisodesQuery>,
) -> (StatusCode, Json<ApiResponse<Vec<EpisodeItem>>>) {
    let bangumi_id_str = bangumi_id.to_string();
    let force = query.force.unwrap_or(false);

    let easy_id = match sqlx::query_scalar!(
        "SELECT id FROM bangumi_info_easy WHERE external_id = ?",
        bangumi_id_str
    )
    .fetch_optional(&pool)
    .await
    {
        Ok(Some(id)) => id,
        Ok(None) => return not_found("Bangumi not found"),
        Err(e) => {
            log::error!("DB error: {:?}", e);
            return internal_error("Database error");
        }
    };

    if let Err(error) =
        ensure_episode_metadata_cached(&pool, easy_id, bangumi_id_str.as_str(), force).await
    {
        log::warn!(
            "Unable to refresh episode metadata for Bangumi {}: {}",
            bangumi_id,
            error
        );
    }

    let recording_id = match sqlx::query_scalar!(
        r#"
        SELECT r.id
        FROM recordings r
        WHERE r.bangumi_id = ? AND r.user_id = ? AND r.is_delete = 0
        "#,
        easy_id,
        auth_user.user_id
    )
    .fetch_optional(&pool)
    .await
    {
        Ok(Some(id)) => id,
        Ok(None) => 0,
        Err(e) => {
            log::error!("DB error: {:?}", e);
            return internal_error("Database error");
        }
    };

    let metadata = match sqlx::query_as::<
        _,
        (
            i32,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<NaiveDate>,
            Option<String>,
        ),
    >(
        r#"
        SELECT ordinal, ep_label, title, name_cn, airdate, duration
        FROM bangumi_episodes
        WHERE bangumi_easy_id = ?
        ORDER BY ordinal ASC
        "#,
    )
    .bind(easy_id)
    .fetch_all(&pool)
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            log::error!("DB error: {:?}", e);
            return internal_error("Database error");
        }
    };

    let user_records = if recording_id == 0 {
        Vec::new()
    } else {
        match sqlx::query!(
            r#"
            SELECT ordinal, watched, progress_seconds, duration_seconds, completed_at, updated_at
            FROM episode_records
            WHERE recording_id = ?
            "#,
            recording_id
        )
        .fetch_all(&pool)
        .await
        {
            Ok(rows) => rows,
            Err(e) => {
                log::error!("DB error: {:?}", e);
                return internal_error("Database error");
            }
        }
    };

    let user_map: std::collections::HashMap<i32, _> =
        user_records.iter().map(|r| (r.ordinal, r)).collect();

    let metadata_map: std::collections::HashMap<i32, _> =
        metadata.into_iter().map(|m| (m.0, m)).collect();

    let mut seen_ordinals: std::collections::HashSet<i32> = std::collections::HashSet::new();
    let mut episodes: Vec<EpisodeItem> = Vec::new();

    // First pass: metadata entries merged with user records
    for (&ordinal, m) in &metadata_map {
        let user_state = user_map.get(&ordinal);
        seen_ordinals.insert(ordinal);
        episodes.push(EpisodeItem {
            ordinal,
            label: m.1.clone(),
            title: m.2.clone(),
            name_cn: m.3.clone(),
            airdate: m.4,
            duration: m.5.clone(),
            watched: user_state.map(|u| u.watched != 0).unwrap_or(false),
            progress_seconds: user_state.and_then(|u| u.progress_seconds),
            duration_seconds: user_state.and_then(|u| u.duration_seconds),
            completed_at: user_state.and_then(|u| u.completed_at),
            updated_at: user_state.map(|u| u.updated_at),
        });
    }

    // Second pass: user records without metadata (e.g. scraping hasn't run yet)
    for r in &user_records {
        if seen_ordinals.contains(&r.ordinal) {
            continue;
        }
        episodes.push(EpisodeItem {
            ordinal: r.ordinal,
            label: None,
            title: None,
            name_cn: None,
            airdate: None,
            duration: None,
            watched: r.watched != 0,
            progress_seconds: r.progress_seconds,
            duration_seconds: r.duration_seconds,
            completed_at: r.completed_at,
            updated_at: Some(r.updated_at),
        });
    }

    episodes.sort_by_key(|e| e.ordinal);

    success(episodes)
}

#[derive(Debug, PartialEq)]
struct EpisodeOrdinalResolution {
    resolved: i32,
    compatibility_mapped: bool,
}

fn resolve_episode_ordinal_from_metadata(
    requested: i32,
    metadata: &[(i32, bool)],
) -> EpisodeOrdinalResolution {
    let active_ordinals = metadata
        .iter()
        .filter_map(|(ordinal, is_stale)| (!is_stale).then_some(*ordinal))
        .collect::<HashSet<_>>();
    if active_ordinals.contains(&requested) {
        return EpisodeOrdinalResolution {
            resolved: requested,
            compatibility_mapped: false,
        };
    }

    let active_regular_ordinals = active_ordinals
        .iter()
        .copied()
        .filter(|ordinal| *ordinal > 0)
        .collect::<std::collections::BTreeSet<_>>();
    if requested > 0
        && active_regular_ordinals
            .first()
            .is_some_and(|first| *first > active_regular_ordinals.len() as i32)
        && let Some(resolved) = active_regular_ordinals.iter().nth((requested - 1) as usize)
    {
        return EpisodeOrdinalResolution {
            resolved: *resolved,
            compatibility_mapped: true,
        };
    }

    // Keep a stale exact match usable during the one-fetch grace period. It will
    // disappear only after two complete snapshots both omit it.
    EpisodeOrdinalResolution {
        resolved: requested,
        compatibility_mapped: false,
    }
}

async fn resolve_episode_ordinal(
    pool: &MySqlPool,
    easy_id: u32,
    bangumi_id: &str,
    requested: i32,
) -> Result<EpisodeOrdinalResolution, sqlx::Error> {
    let load_metadata = || async {
        sqlx::query_as::<_, (i32, i8)>(
            "SELECT ordinal, is_stale FROM bangumi_episodes \
             WHERE bangumi_easy_id = ? ORDER BY ordinal ASC",
        )
        .bind(easy_id)
        .fetch_all(pool)
        .await
    };

    let mut rows = load_metadata().await?;
    if rows.is_empty() {
        if let Err(error) = ensure_episode_metadata_cached(pool, easy_id, bangumi_id, false).await {
            log::warn!(
                "Unable to populate episode metadata for compatibility mapping of Bangumi {}: {}",
                bangumi_id,
                error
            );
        }
        rows = load_metadata().await?;
    }
    let metadata = rows
        .into_iter()
        .map(|(ordinal, is_stale)| (ordinal, is_stale != 0))
        .collect::<Vec<_>>();
    Ok(resolve_episode_ordinal_from_metadata(requested, &metadata))
}

pub async fn update_episode(
    State(pool): State<MySqlPool>,
    Extension(auth_user): Extension<AuthUser>,
    Path((bangumi_id, ordinal)): Path<(u32, i32)>,
    Json(body): Json<UpdateEpisodeBody>,
) -> (StatusCode, Json<ApiResponse<EpisodeItem>>) {
    let bangumi_id_str = bangumi_id.to_string();

    let recording = match sqlx::query!(
        r#"
        SELECT r.id, b.id AS bangumi_easy_id, r.recorder
        FROM recordings r
        JOIN bangumi_info_easy b ON r.bangumi_id = b.id
        WHERE b.external_id = ? AND r.user_id = ?
        "#,
        bangumi_id_str,
        auth_user.user_id
    )
    .fetch_optional(&pool)
    .await
    {
        Ok(Some(row)) => row,
        Ok(None) => return not_found("Recording not found for this bangumi"),
        Err(e) => {
            log::error!("DB error: {:?}", e);
            return internal_error("Database error");
        }
    };
    let recording_id = recording.id;
    let ordinal_resolution =
        match resolve_episode_ordinal(&pool, recording.bangumi_easy_id, &bangumi_id_str, ordinal)
            .await
        {
            Ok(resolution) => resolution,
            Err(error) => {
                log::error!("Failed to resolve episode ordinal: {error}");
                return internal_error("Database error");
            }
        };
    let resolved_ordinal = ordinal_resolution.resolved;

    let old_episode = match sqlx::query!(
        r#"
        SELECT watched, progress_seconds, duration_seconds, completed_at
        FROM episode_records
        WHERE recording_id = ? AND ordinal = ?
        "#,
        recording_id,
        resolved_ordinal
    )
    .fetch_optional(&pool)
    .await
    {
        Ok(row) => row,
        Err(e) => {
            log::error!("DB error: {:?}", e);
            return internal_error("Database error");
        }
    };

    let watched = body.watched.unwrap_or(false);
    let progress_seconds = body.progress_seconds;
    let duration_seconds = body.duration_seconds;
    let now = chrono::Utc::now().naive_utc();
    let completed_at = if watched { Some(now) } else { None };

    match sqlx::query!(
        r#"
        INSERT INTO episode_records (recording_id, ordinal, watched, progress_seconds, duration_seconds, completed_at, updated_at)
        VALUES (?, ?, ?, ?, ?, ?, ?)
        ON DUPLICATE KEY UPDATE
            watched = VALUES(watched),
            progress_seconds = VALUES(progress_seconds),
            duration_seconds = VALUES(duration_seconds),
            completed_at = VALUES(completed_at),
            updated_at = VALUES(updated_at)
        "#,
        recording_id,
        resolved_ordinal,
        watched as i8,
        progress_seconds,
        duration_seconds,
        completed_at,
        now
    )
    .execute(&pool)
    .await
    {
        Ok(_) => {}
        Err(e) => {
            log::error!("DB error: {:?}", e);
            return internal_error("Failed to update episode");
        }
    };

    let updated = match sqlx::query!(
        r#"
        SELECT ordinal, watched, progress_seconds, duration_seconds, completed_at, updated_at
        FROM episode_records
        WHERE recording_id = ? AND ordinal = ?
        "#,
        recording_id,
        resolved_ordinal
    )
    .fetch_optional(&pool)
    .await
    {
        Ok(Some(r)) => EpisodeItem {
            ordinal: r.ordinal,
            label: None,
            title: None,
            name_cn: None,
            airdate: None,
            duration: None,
            watched: r.watched != 0,
            progress_seconds: r.progress_seconds,
            duration_seconds: r.duration_seconds,
            completed_at: r.completed_at,
            updated_at: Some(r.updated_at),
        },
        Ok(None) => {
            return internal_error("Episode record not found after insert");
        }
        Err(e) => {
            log::error!("DB error: {:?}", e);
            return internal_error("Database error");
        }
    };

    let old_episode_value = old_episode.as_ref().map(|old| {
        json!({
            "watched": old.watched != 0,
            "progress_seconds": old.progress_seconds,
            "duration_seconds": old.duration_seconds,
            "completed_at": old.completed_at,
        })
    });
    let new_episode_value = json!({
        "watched": updated.watched,
        "progress_seconds": updated.progress_seconds,
        "duration_seconds": updated.duration_seconds,
        "completed_at": updated.completed_at,
    });
    if episode_has_user_visible_change(
        old_episode
            .as_ref()
            .is_some_and(|episode| episode.watched != 0),
        old_episode
            .as_ref()
            .and_then(|episode| episode.progress_seconds),
        updated.watched,
        updated.progress_seconds,
    ) {
        write_recording_log(
            &pool,
            recording_id,
            Some(auth_user.user_id),
            LogTarget::Bangumi(recording.bangumi_easy_id),
            if old_episode.is_some() {
                "episode_updated"
            } else {
                "episode_created"
            },
            None,
            old_episode_value,
            Some(new_episode_value),
            Some(json!({
                "ordinal": resolved_ordinal,
                "requested_ordinal": ordinal,
                "compatibility_mapped": ordinal_resolution.compatibility_mapped,
                "bangumi_id": bangumi_id,
            })),
        )
        .await;
    }

    // Sync main recordings.recorder field: "{max_watched_ordinal}|{mm:ss}"
    let count = sqlx::query_scalar!(
        r#"
        SELECT MAX(ordinal) FROM episode_records
        WHERE recording_id = ? AND watched = 1
        "#,
        recording_id
    )
    .fetch_one(&pool)
    .await
    .unwrap_or(None)
    .unwrap_or(0);

    let max_progress = sqlx::query_scalar!(
        r#"
        SELECT MAX(progress_seconds) FROM episode_records
        WHERE recording_id = ?
        "#,
        recording_id
    )
    .fetch_one(&pool)
    .await
    .unwrap_or(None);

    let time_str = match max_progress {
        Some(s) if s > 0 => format_progress_seconds(s),
        _ => "0:00".to_string(),
    };
    let new_recorder = format!("{}|{}", count, time_str);

    if let Err(e) = sqlx::query!(
        "UPDATE recordings SET recorder = ?, updated_at = ? WHERE id = ?",
        new_recorder,
        now,
        recording_id
    )
    .execute(&pool)
    .await
    {
        log::error!("Failed to sync recorder after episode update: {:?}", e);
    }

    if recording.recorder.as_deref() != Some(new_recorder.as_str()) {
        write_recording_log(
            &pool,
            recording_id,
            Some(auth_user.user_id),
            LogTarget::Bangumi(recording.bangumi_easy_id),
            "recorder_changed",
            Some("recorder"),
            recording.recorder.map(|v| json!(v)),
            Some(json!(new_recorder)),
            Some(json!({
                "source": "episode_update",
                "ordinal": resolved_ordinal,
                "requested_ordinal": ordinal,
                "compatibility_mapped": ordinal_resolution.compatibility_mapped,
            })),
        )
        .await;
    }

    if ordinal_resolution.compatibility_mapped {
        let warning = format!(
            "Warning: requested episode ordinal {} was mapped to Bangumi subject ordinal {}. Clients should send the subject ordinal directly.",
            ordinal, resolved_ordinal
        );
        log::warn!(
            "Mapped episode ordinal {} to {} for Bangumi {} user {}",
            ordinal,
            resolved_ordinal,
            bangumi_id,
            auth_user.user_id
        );
        write_system_log(
            &pool,
            "warn",
            "episode_api",
            "episode_ordinal_compatibility_mapped",
            &warning,
            Some(auth_user.user_id),
            Some(json!({
                "subject_id": bangumi_id,
                "requested_ordinal": ordinal,
                "resolved_ordinal": resolved_ordinal,
                "recording_id": recording_id,
            })),
        )
        .await;
        success_with_message(updated, warning)
    } else {
        success(updated)
    }
}

pub fn format_progress_seconds(sec: i32) -> String {
    let m = sec / 60;
    let s = sec % 60;
    format!("{}:{:02}", m, s)
}

/// Parsed episode data from bangumi subject page.
#[derive(Clone, Debug, PartialEq)]
pub struct ParsedEpisode {
    pub ordinal: i32,
    pub ep_id: String,
    pub label: String,
    pub title: Option<String>,
    pub name_cn: Option<String>,
    pub airdate: Option<NaiveDate>,
    pub duration: Option<String>,
}

/// Extract the first `HH:MM:SS` pattern from a string.
fn extract_time_hms(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    for i in 0..bytes.len().saturating_sub(7) {
        if bytes[i + 2] == b':'
            && bytes[i + 5] == b':'
            && bytes[i..i + 2].iter().all(|b| b.is_ascii_digit())
            && bytes[i + 3..i + 5].iter().all(|b| b.is_ascii_digit())
            && bytes[i + 6..i + 8].iter().all(|b| b.is_ascii_digit())
        {
            return Some(s[i..i + 8].to_string());
        }
    }
    None
}

/// Parse bangumi /ep page HTML to extract episode metadata from `ul.line_list > li`.
/// Handles both TV (regular + SP episodes) and 剧场版 (movie) formats.
/// SP episodes are stored with negative ordinals (SP1 -> -1) but expose labels like "SP1".
pub fn parse_ep_page_episodes(html: &str) -> Vec<ParsedEpisode> {
    let document = Html::parse_document(html);

    static LI_SEL: OnceLock<Selector> = OnceLock::new();
    static A_SEL: OnceLock<Selector> = OnceLock::new();
    static TIP_SEL: OnceLock<Selector> = OnceLock::new();
    static SMALL_SEL: OnceLock<Selector> = OnceLock::new();

    let li_sel = selector("ul.line_list > li", &LI_SEL);
    let a_sel = selector("h6 a", &A_SEL);
    let tip_sel = selector("h6 span.tip", &TIP_SEL);
    let small_sel = selector("small.grey", &SMALL_SEL);

    let mut episodes = Vec::new();

    for li in document.select(li_sel) {
        // Skip category header rows (li.cat), e.g. "本篇", "特别篇"
        if li.value().classes().any(|c| c == "cat") {
            continue;
        }

        let a = match li.select(a_sel).next() {
            Some(a) => a,
            None => continue,
        };

        let href = a.value().attr("href").unwrap_or("");
        let ep_id = href.strip_prefix("/ep/").unwrap_or("").to_string();
        if ep_id.is_empty() {
            continue;
        }

        let a_text = a.text().collect::<String>().trim().to_string();
        // a_text examples: "1.鳥白島へようこそ" or "SP1.劇場編集版 久島鴎編"

        // Parse ordinal, skipping decimal ordinals like "11.5".
        // First check if the ordinal part is a decimal (second segment starts with digit).
        {
            let mut parts = a_text.split('.');
            let decimal_part = parts.nth(1);
            if parts.next().is_some()
                && decimal_part
                    .unwrap_or_default()
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_digit())
            {
                // Decimal ordinal like "11.5.xxx" — skip
                continue;
            }
        }

        // Extract label (the part before the first dot, e.g. "1", "SP1")
        let label = a_text.split('.').next().unwrap_or("").trim().to_string();

        // Internal ordinal: regular episodes keep their number, SP episodes use negative (SP1 -> -1)
        let ordinal = if label.starts_with("SP") {
            let sp_num = label
                .strip_prefix("SP")
                .and_then(|s| s.parse::<i32>().ok())
                .unwrap_or(0);
            if sp_num == 0 {
                continue;
            }
            -sp_num
        } else {
            label.parse::<i32>().unwrap_or(0)
        };

        if ordinal == 0 {
            continue;
        }

        // Parse Japanese title (text after the ordinal prefix and dot)
        let title = {
            let after_dot = a_text
                .find('.')
                .map(|pos| a_text[pos + 1..].trim().to_string())
                .unwrap_or_default();
            if after_dot.is_empty() {
                None
            } else {
                Some(after_dot)
            }
        };

        // Parse Chinese name from <span class="tip"> / 中文标题</span>
        let name_cn = li
            .select(tip_sel)
            .next()
            .map(|s| s.text().collect::<String>())
            .map(|t| t.trim().trim_start_matches('/').trim().to_string())
            .filter(|t| !t.is_empty());

        // Parse metadata from <small class="grey"> elements
        let mut airdate: Option<NaiveDate> = None;
        let mut duration: Option<String> = None;

        for small in li.select(small_sel) {
            let text = small.text().collect::<String>();

            // TV: "时长:00:23:40 / 首播:2025-04-07"
            // Movie/SP: "首播:2025-08-15"
            if let Some(dur_str) = text
                .split("时长:")
                .nth(1)
                .and_then(|s| s.split('/').next())
                .map(|s| s.trim())
            {
                duration = extract_time_hms(dur_str);
            }

            if let Some(date_str) = text
                .split("首播:")
                .nth(1)
                .and_then(|s| s.split('/').next())
                .map(|s| s.trim())
            {
                airdate = NaiveDate::parse_from_str(date_str, "%Y-%m-%d").ok();
            }
        }

        episodes.push(ParsedEpisode {
            ordinal,
            ep_id,
            label,
            title,
            name_cn,
            airdate,
            duration,
        });
    }

    episodes
}

fn validate_episode_snapshot(html: &str, episodes: &[ParsedEpisode]) -> Result<(), String> {
    static LIST_SEL: OnceLock<Selector> = OnceLock::new();
    let document = Html::parse_document(html);
    if document
        .select(selector("ul.line_list", &LIST_SEL))
        .next()
        .is_none()
    {
        return Err("episode page does not contain an episode list".to_string());
    }
    if episodes.is_empty() {
        return Err("episode page did not contain any supported episodes".to_string());
    }

    let mut ordinals = HashSet::with_capacity(episodes.len());
    let mut episode_ids = HashSet::with_capacity(episodes.len());
    for episode in episodes {
        if !ordinals.insert(episode.ordinal) {
            return Err(format!(
                "episode page contains duplicate ordinal {}",
                episode.ordinal
            ));
        }
        if !episode
            .ep_id
            .chars()
            .all(|character| character.is_ascii_digit())
        {
            return Err(format!(
                "episode page contains invalid episode id {}",
                episode.ep_id
            ));
        }
        if !episode_ids.insert(episode.ep_id.as_str()) {
            return Err(format!(
                "episode page contains duplicate episode id {}",
                episode.ep_id
            ));
        }
    }
    Ok(())
}

fn cache_snapshot_is_usable(
    episode_count: i64,
    incomplete_count: i64,
    last_updated_at: Option<NaiveDateTime>,
    now: NaiveDateTime,
) -> bool {
    episode_count > 0
        && incomplete_count == 0
        && last_updated_at.is_some_and(|timestamp| {
            now - timestamp <= chrono::Duration::hours(24)
                && timestamp <= now + chrono::Duration::minutes(5)
        })
}

async fn persist_episode_snapshot(
    pool: &MySqlPool,
    easy_id: u32,
    bangumi_id: &str,
    episodes: &[ParsedEpisode],
    generation: &str,
) -> Result<(), String> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|error| format!("failed to begin episode cache transaction: {error}"))?;

    // Serialise refreshes for one subject so concurrent generations cannot make
    // each other's freshly-upserted rows stale.
    sqlx::query("SELECT id FROM bangumi_info_easy WHERE id = ? FOR UPDATE")
        .bind(easy_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|error| format!("failed to lock Bangumi subject {bangumi_id}: {error}"))?;

    let mut qb = QueryBuilder::new(
        "INSERT INTO bangumi_episodes (bangumi_easy_id, ordinal, ep_label, title, name_cn, airdate, duration, fetch_generation, is_stale, missing_fetch_count) ",
    );
    qb.push_values(episodes.iter(), |mut builder, episode| {
        builder
            .push_bind(easy_id)
            .push_bind(episode.ordinal)
            .push_bind(&episode.label)
            .push_bind(&episode.title)
            .push_bind(&episode.name_cn)
            .push_bind(episode.airdate)
            .push_bind(&episode.duration)
            .push_bind(generation)
            .push_bind(false)
            .push_bind(0_u8);
    });
    qb.push(
        " ON DUPLICATE KEY UPDATE ep_label = VALUES(ep_label), title = VALUES(title), \
         name_cn = VALUES(name_cn), airdate = VALUES(airdate), duration = VALUES(duration), \
         fetch_generation = VALUES(fetch_generation), is_stale = 0, missing_fetch_count = 0",
    );
    qb.build()
        .execute(&mut *tx)
        .await
        .map_err(|error| format!("failed to upsert episode cache snapshot: {error}"))?;

    sqlx::query(
        r#"UPDATE bangumi_episodes
           SET is_stale = 1,
               missing_fetch_count = LEAST(missing_fetch_count + 1, 255)
           WHERE bangumi_easy_id = ?
             AND (fetch_generation IS NULL OR fetch_generation <> ?)"#,
    )
    .bind(easy_id)
    .bind(generation)
    .execute(&mut *tx)
    .await
    .map_err(|error| format!("failed to mark missing episode metadata stale: {error}"))?;

    let deleted_ordinals = sqlx::query_scalar::<_, i32>(
        r#"SELECT ordinal
           FROM bangumi_episodes
           WHERE bangumi_easy_id = ? AND is_stale = 1 AND missing_fetch_count >= 2
           ORDER BY ordinal ASC"#,
    )
    .bind(easy_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(|error| format!("failed to inspect stale episode metadata: {error}"))?;

    if !deleted_ordinals.is_empty() {
        sqlx::query(
            r#"DELETE FROM bangumi_episodes
               WHERE bangumi_easy_id = ? AND is_stale = 1 AND missing_fetch_count >= 2"#,
        )
        .bind(easy_id)
        .execute(&mut *tx)
        .await
        .map_err(|error| format!("failed to clean stale episode metadata: {error}"))?;
    }

    tx.commit()
        .await
        .map_err(|error| format!("failed to commit episode cache snapshot: {error}"))?;

    if !deleted_ordinals.is_empty() {
        write_system_log(
            pool,
            "info",
            "episode_cache",
            "stale_episode_metadata_cleaned",
            &format!(
                "Cleaned {} stale episode metadata row(s) for Bangumi Subject {}",
                deleted_ordinals.len(),
                bangumi_id
            ),
            None,
            Some(json!({
                "cleanup_count": deleted_ordinals.len(),
                "subject_id": bangumi_id,
                "ordinals": deleted_ordinals,
                "fetch_generation": generation,
            })),
        )
        .await;
    }

    Ok(())
}

pub(crate) async fn ensure_episode_metadata_cached(
    pool: &MySqlPool,
    easy_id: u32,
    bangumi_id: &str,
    force: bool,
) -> Result<(), String> {
    if !force {
        let cache_state = sqlx::query(
            r#"SELECT
                   COUNT(*) AS episode_count,
                   COALESCE(SUM(
                       CASE WHEN title IS NULL OR TRIM(title) = ''
                                  OR name_cn IS NULL OR TRIM(name_cn) = ''
                            THEN 1 ELSE 0 END
                   ), 0) AS incomplete_count,
                   MAX(updated_at) AS last_updated_at
               FROM bangumi_episodes
               WHERE bangumi_easy_id = ? AND is_stale = 0"#,
        )
        .bind(easy_id)
        .fetch_one(pool)
        .await
        .map_err(|error| format!("failed to inspect episode cache: {error}"))?;
        let episode_count = cache_state.try_get::<i64, _>("episode_count").unwrap_or(0);
        let incomplete_count = cache_state
            .try_get::<i64, _>("incomplete_count")
            .unwrap_or(episode_count);
        let last_updated_at = cache_state
            .try_get::<Option<NaiveDateTime>, _>("last_updated_at")
            .unwrap_or(None);
        if cache_snapshot_is_usable(
            episode_count,
            incomplete_count,
            last_updated_at,
            Utc::now().naive_utc(),
        ) {
            return Ok(());
        }
    }

    let url = format!("https://bangumi.tv/subject/{}/ep", bangumi_id);
    let resp = http_client()
        .get(&url)
        .send()
        .await
        .map_err(|error| format!("failed to fetch episode metadata page: {error}"))?
        .error_for_status()
        .map_err(|error| format!("episode metadata page returned an error: {error}"))?;

    let html = resp
        .text()
        .await
        .map_err(|error| format!("failed to decode episode metadata page: {error}"))?;

    let episodes = parse_ep_page_episodes(&html);
    validate_episode_snapshot(&html, &episodes)?;

    let generation = uuid::Uuid::new_v4().to_string();
    persist_episode_snapshot(pool, easy_id, bangumi_id, &episodes, &generation).await
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_EPISODE_HTML: &str = r#"
        <ul class="line_list">
            <li><h6><a href="/ep/1459757">74.第四季第一集</a></h6></li>
            <li><h6><a href="/ep/1459758">75.第四季第二集</a></h6></li>
        </ul>
    "#;

    #[test]
    fn compatibility_maps_season_relative_ordinals_to_subject_ordinals() {
        let metadata = [(74, false), (75, false), (76, false)];
        assert_eq!(
            resolve_episode_ordinal_from_metadata(1, &metadata),
            EpisodeOrdinalResolution {
                resolved: 74,
                compatibility_mapped: true,
            }
        );
        assert_eq!(
            resolve_episode_ordinal_from_metadata(2, &metadata),
            EpisodeOrdinalResolution {
                resolved: 75,
                compatibility_mapped: true,
            }
        );
    }

    #[test]
    fn compatibility_prefers_exact_and_preserves_unmappable_ordinals() {
        let metadata = [(1, true), (74, false), (75, false), (-1, false)];
        assert_eq!(
            resolve_episode_ordinal_from_metadata(74, &metadata),
            EpisodeOrdinalResolution {
                resolved: 74,
                compatibility_mapped: false,
            }
        );
        assert_eq!(
            resolve_episode_ordinal_from_metadata(1, &metadata),
            EpisodeOrdinalResolution {
                resolved: 74,
                compatibility_mapped: true,
            }
        );
        assert_eq!(
            resolve_episode_ordinal_from_metadata(-1, &metadata),
            EpisodeOrdinalResolution {
                resolved: -1,
                compatibility_mapped: false,
            }
        );
        assert_eq!(
            resolve_episode_ordinal_from_metadata(4, &metadata),
            EpisodeOrdinalResolution {
                resolved: 4,
                compatibility_mapped: false,
            }
        );
    }

    #[test]
    fn compatibility_does_not_remap_a_temporarily_stale_first_episode() {
        let metadata = [(1, true), (2, false), (3, false), (4, false), (5, false)];
        assert_eq!(
            resolve_episode_ordinal_from_metadata(1, &metadata),
            EpisodeOrdinalResolution {
                resolved: 1,
                compatibility_mapped: false,
            }
        );
    }

    #[test]
    fn complete_episode_cache_uses_24_hour_ttl() {
        let now = NaiveDate::from_ymd_opt(2026, 7, 14)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap();
        assert!(cache_snapshot_is_usable(
            2,
            0,
            Some(now - chrono::Duration::hours(23)),
            now,
        ));
        assert!(!cache_snapshot_is_usable(
            2,
            0,
            Some(now - chrono::Duration::hours(25)),
            now,
        ));
    }

    #[test]
    fn incomplete_episode_cache_is_refreshed() {
        let now = NaiveDate::from_ymd_opt(2026, 7, 14)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap();
        assert!(!cache_snapshot_is_usable(2, 1, Some(now), now));
        assert!(!cache_snapshot_is_usable(0, 0, Some(now), now));
    }

    #[test]
    fn complete_snapshot_validation_rejects_partial_or_ambiguous_results() {
        let episodes = parse_ep_page_episodes(VALID_EPISODE_HTML);
        assert!(validate_episode_snapshot(VALID_EPISODE_HTML, &episodes).is_ok());
        assert!(validate_episode_snapshot("<html></html>", &[]).is_err());

        let mut duplicates = episodes.clone();
        duplicates.push(episodes[0].clone());
        assert!(validate_episode_snapshot(VALID_EPISODE_HTML, &duplicates).is_err());
    }

    #[test]
    fn test_format_progress_seconds() {
        assert_eq!(format_progress_seconds(0), "0:00");
        assert_eq!(format_progress_seconds(1), "0:01");
        assert_eq!(format_progress_seconds(59), "0:59");
        assert_eq!(format_progress_seconds(60), "1:00");
        assert_eq!(format_progress_seconds(61), "1:01");
        assert_eq!(format_progress_seconds(120), "2:00");
        assert_eq!(format_progress_seconds(3661), "61:01");
    }

    #[test]
    fn test_extract_time_hms() {
        assert_eq!(extract_time_hms("00:52:40").as_deref(), Some("00:52:40"));
        assert_eq!(
            extract_time_hms("00:52:40讨论 (+20)").as_deref(),
            Some("00:52:40")
        );
        assert_eq!(
            extract_time_hms("01:30:30讨论 (+214)").as_deref(),
            Some("01:30:30")
        );
        assert_eq!(
            extract_time_hms("00:52:40讨论").as_deref(),
            Some("00:52:40")
        );
        assert_eq!(extract_time_hms("no time here"), None);
        assert_eq!(extract_time_hms("12:34"), None);
        assert_eq!(extract_time_hms(""), None);
    }

    #[test]
    fn test_parse_ep_page_tv_regular() {
        // TV regular episodes with both 时长 and 首播
        let html = r#"
        <ul class="line_list">
            <li class="cat">本篇</li>
            <li class="line_odd">
                <h6>
                    <a href="/ep/1459757">1.鳥白島へようこそ</a>
                    <span class="tip"> / 欢迎来到鸟白岛</span>
                </h6>
                <small class="grey">时长:00:23:40 / 首播:2025-04-07</small>
                <small class="grey">/ 讨论:+363</small>
            </li>
            <li class="line_even">
                <h6>
                    <a href="/ep/1459758">2.夏休みの過ごし方</a>
                    <span class="tip"> / 度过暑假的方法</span>
                </h6>
                <small class="grey">时长:00:23:40 / 首播:2025-04-14</small>
                <small class="grey">/ 讨论:+289</small>
            </li>
        </ul>
        "#;
        let result = parse_ep_page_episodes(html);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].ordinal, 1);
        assert_eq!(result[0].label, "1");
        assert_eq!(result[0].title.as_deref(), Some("鳥白島へようこそ"));
        assert_eq!(result[0].name_cn.as_deref(), Some("欢迎来到鸟白岛"));
        assert_eq!(
            result[0].airdate,
            Some(NaiveDate::from_ymd_opt(2025, 4, 7).unwrap())
        );
        assert_eq!(result[0].duration.as_deref(), Some("00:23:40"));
        assert_eq!(result[0].ep_id, "1459757");
        assert_eq!(result[1].ordinal, 2);
        assert_eq!(result[1].label, "2");
        assert_eq!(result[1].title.as_deref(), Some("夏休みの過ごし方"));
        assert_eq!(result[1].name_cn.as_deref(), Some("度过暑假的方法"));
        assert_eq!(
            result[1].airdate,
            Some(NaiveDate::from_ymd_opt(2025, 4, 14).unwrap())
        );
    }

    #[test]
    fn test_parse_ep_page_sp_episodes() {
        // SP episodes: no Chinese title, no 时长
        let html = r#"
        <ul class="line_list">
            <li class="cat">特别篇</li>
            <li class="line_odd">
                <h6>
                    <a href="/ep/1530061">SP1.劇場編集版 久島鴎編</a>
                </h6>
                <small class="grey">首播:2025-08-15</small>
                <small class="grey">/ 讨论:+7</small>
            </li>
            <li class="line_even">
                <h6>
                    <a href="/ep/1535302">SP2.劇場編集版 紬ヴェンダース編</a>
                </h6>
                <small class="grey">首播:2025-08-22</small>
                <small class="grey">/ 讨论:+8</small>
            </li>
        </ul>
        "#;
        let result = parse_ep_page_episodes(html);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].ordinal, -1);
        assert_eq!(result[0].label, "SP1");
        assert_eq!(result[0].title.as_deref(), Some("劇場編集版 久島鴎編"));
        assert_eq!(result[0].name_cn, None);
        assert_eq!(
            result[0].airdate,
            Some(NaiveDate::from_ymd_opt(2025, 8, 15).unwrap())
        );
        assert_eq!(result[0].duration, None);
        assert_eq!(result[1].ordinal, -2);
        assert_eq!(result[1].label, "SP2");
        assert_eq!(
            result[1].title.as_deref(),
            Some("劇場編集版 紬ヴェンダース編")
        );
    }

    #[test]
    fn test_parse_ep_page_movie() {
        // 剧场版 (movie): single episode, only 首播, no 时长
        let html = r#"
        <ul class="line_list">
            <li class="cat">本篇</li>
            <li class="line_odd">
                <h6>
                    <a href="/ep/658319">1.君の名は</a>
                    <span class="tip"> / 你的名字</span>
                </h6>
                <small class="grey">首播:2016-08-26</small>
                <small class="grey">/ 讨论:+77</small>
            </li>
        </ul>
        "#;
        let result = parse_ep_page_episodes(html);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].ordinal, 1);
        assert_eq!(result[0].label, "1");
        assert_eq!(result[0].title.as_deref(), Some("君の名は"));
        assert_eq!(result[0].name_cn.as_deref(), Some("你的名字"));
        assert_eq!(
            result[0].airdate,
            Some(NaiveDate::from_ymd_opt(2016, 8, 26).unwrap())
        );
        assert_eq!(result[0].duration, None);
    }

    #[test]
    fn test_parse_ep_page_skips_cat() {
        // Category headers like "本篇", "特别篇" should be skipped
        let html = r#"
        <ul class="line_list">
            <li class="cat">本篇</li>
            <li class="cat">特别篇</li>
            <li class="line_odd">
                <h6>
                    <a href="/ep/1459757">1.First</a>
                    <span class="tip"> / 第一</span>
                </h6>
                <small class="grey">时长:00:23:40 / 首播:2025-04-07</small>
            </li>
        </ul>
        "#;
        let result = parse_ep_page_episodes(html);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].ordinal, 1);
        assert_eq!(result[0].label, "1");
    }

    #[test]
    fn test_parse_ep_page_decimal_ordinal_skipped() {
        // Decimal ordinals should be skipped (same as before)
        let html = r#"
        <ul class="line_list">
            <li class="line_odd">
                <h6>
                    <a href="/ep/925285">11.5.Memory Snow</a>
                </h6>
                <small class="grey">首播:2020-02-12</small>
            </li>
        </ul>
        "#;
        let result = parse_ep_page_episodes(html);
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_ep_page_airdate_only() {
        // Episodes with only 首播 (no 时长) - common for movies and SPs
        let html = r#"
        <ul class="line_list">
            <li class="line_odd">
                <h6>
                    <a href="/ep/123456">1.No Duration</a>
                    <span class="tip"> / 无时长</span>
                </h6>
                <small class="grey">首播:2024-01-15</small>
                <small class="grey">/ 讨论:+10</small>
            </li>
        </ul>
        "#;
        let result = parse_ep_page_episodes(html);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].label, "1");
        assert_eq!(
            result[0].airdate,
            Some(NaiveDate::from_ymd_opt(2024, 1, 15).unwrap())
        );
        assert_eq!(result[0].duration, None);
    }

    /// Opt-in stateful test. The database must be disposable because this runs all migrations.
    #[tokio::test]
    async fn live_mariadb_snapshot_requires_two_misses_and_preserves_episode_records() {
        let Ok(database_url) = std::env::var("BR_EPISODE_CACHE_TEST_DATABASE_URL") else {
            return;
        };
        let pool = MySqlPool::connect(&database_url).await.unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();

        let unique = uuid::Uuid::new_v4();
        let bangumi_id = (800_000_000 + (unique.as_u128() % 90_000_000) as u32).to_string();
        let username = format!("ep-cache-{unique}");
        let user_id = sqlx::query(
            "INSERT INTO users (username, password_hash, api_token_hash, uuid) \
             VALUES (?, 'unused', ?, ?)",
        )
        .bind(username)
        .bind(format!("{:064x}", unique.as_u128()))
        .bind(unique.to_string())
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_id() as i64;
        let easy_id = sqlx::query(
            "INSERT INTO bangumi_info_easy (external_id, title, type) VALUES (?, 'Snapshot test', 1)",
        )
        .bind(&bangumi_id)
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_id() as u32;
        let recording_id =
            sqlx::query("INSERT INTO recordings (user_id, bangumi_id, status) VALUES (?, ?, 1)")
                .bind(user_id)
                .bind(easy_id)
                .execute(&pool)
                .await
                .unwrap()
                .last_insert_id() as u32;
        sqlx::query(
            "INSERT INTO episode_records (recording_id, ordinal, watched) VALUES (?, 75, 1)",
        )
        .bind(recording_id)
        .execute(&pool)
        .await
        .unwrap();

        let full_snapshot = parse_ep_page_episodes(VALID_EPISODE_HTML);
        let missing_75 = vec![full_snapshot[0].clone()];
        persist_episode_snapshot(&pool, easy_id, &bangumi_id, &full_snapshot, "generation-1")
            .await
            .unwrap();

        let (status, Json(mapped_response)) = update_episode(
            State(pool.clone()),
            Extension(AuthUser { user_id }),
            Path((bangumi_id.parse().unwrap(), 1)),
            Json(UpdateEpisodeBody {
                watched: Some(true),
                progress_seconds: Some(30),
                duration_seconds: Some(1_440),
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(mapped_response.data.as_ref().unwrap().ordinal, 74);
        assert!(
            mapped_response.message.as_deref().is_some_and(
                |message| message.contains("ordinal 1") && message.contains("ordinal 74")
            )
        );
        let compatibility_warning_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM system_logs \
             WHERE category = 'episode_api' \
               AND action = 'episode_ordinal_compatibility_mapped' \
               AND JSON_UNQUOTE(JSON_EXTRACT(metadata, '$.requested_ordinal')) = '1' \
               AND JSON_UNQUOTE(JSON_EXTRACT(metadata, '$.resolved_ordinal')) = '74'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(compatibility_warning_count, 1);

        persist_episode_snapshot(&pool, easy_id, &bangumi_id, &missing_75, "generation-2")
            .await
            .unwrap();

        let first_miss = sqlx::query(
            "SELECT is_stale, missing_fetch_count FROM bangumi_episodes \
             WHERE bangumi_easy_id = ? AND ordinal = 75",
        )
        .bind(easy_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_ne!(first_miss.get::<i8, _>("is_stale"), 0);
        assert_eq!(first_miss.get::<u8, _>("missing_fetch_count"), 1);

        // Seeing the episode again resets the consecutive-miss state.
        persist_episode_snapshot(&pool, easy_id, &bangumi_id, &full_snapshot, "generation-3")
            .await
            .unwrap();
        persist_episode_snapshot(&pool, easy_id, &bangumi_id, &missing_75, "generation-4")
            .await
            .unwrap();
        persist_episode_snapshot(&pool, easy_id, &bangumi_id, &missing_75, "generation-5")
            .await
            .unwrap();

        let metadata_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM bangumi_episodes WHERE bangumi_easy_id = ? AND ordinal = 75",
        )
        .bind(easy_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        let episode_record_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM episode_records WHERE recording_id = ? AND ordinal = 75",
        )
        .bind(recording_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(metadata_count, 0);
        assert_eq!(episode_record_count, 1);

        let cleanup_metadata: String = sqlx::query_scalar(
            "SELECT CAST(metadata AS CHAR) FROM system_logs \
             WHERE category = 'episode_cache' AND action = 'stale_episode_metadata_cleaned' \
             ORDER BY id DESC LIMIT 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let cleanup_metadata: serde_json::Value = serde_json::from_str(&cleanup_metadata).unwrap();
        assert_eq!(cleanup_metadata["cleanup_count"], 1);
        assert_eq!(cleanup_metadata["subject_id"], bangumi_id);
        assert_eq!(cleanup_metadata["ordinals"], json!([75]));

        sqlx::query(
            "DELETE FROM system_logs \
             WHERE JSON_UNQUOTE(JSON_EXTRACT(metadata, '$.subject_id')) = ?",
        )
        .bind(&bangumi_id)
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
    }
}
