use crate::auth_bearer::AuthUser;
use axum::{
    Json,
    extract::{Extension, Path, Query, State},
    http::StatusCode,
};
use chrono::{Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sqlx::mysql::MySqlPool;

use super::episodes::ensure_episode_metadata_cached;
use super::response::{ApiResponse, bad_request, internal_error, not_found, success};
use crate::api::imdb::{
    ImdbIDSearchQuery, ImdbItem, ImdbSearchItem, ImdbSearchQuery, search_imdb as v1_search_imdb,
    search_imdb_by_id as v1_search_imdb_by_id,
};
use crate::api::search::{
    BangumiSearchItem, IDSearchQuery, LocalSearchItem, OtherItem, TitleSearchQuery,
    get_other_by_id as v1_get_other_by_id, search_bangumi_by_id as v1_search_by_id,
    search_bangumi_by_title as v1_search_title, search_local as v1_search_local,
};

#[derive(Deserialize)]
pub struct EpisodesForceQuery {
    pub force: Option<bool>,
}

pub use crate::api::search::BangumiItem;

#[derive(Deserialize)]
pub struct SearchQuery {
    pub q: Option<String>,
    pub page: Option<i32>,
}

#[derive(Deserialize)]
pub struct BangumiQuery {
    pub force: Option<bool>,
}

#[derive(Deserialize)]
pub struct ImdbQuery {
    pub force: Option<bool>,
    pub use_api: Option<bool>,
}

#[derive(Deserialize)]
pub struct ImdbSearchParams {
    pub q: Option<String>,
    pub page: Option<i32>,
    pub use_api: Option<bool>,
}

#[derive(Deserialize)]
pub struct LocalSearchParams {
    pub q: Option<String>,
    pub id: Option<u32>,
    pub page: Option<i32>,
    pub page_size: Option<i32>,
}

#[derive(Serialize)]
pub struct LocalSearchResult {
    pub items: Vec<LocalSearchItem>,
    pub total: i64,
    pub page: i32,
    pub page_size: i32,
}

/// GET /api/v2/search?q=keyword&page=1
///
/// Forwards to v1 which scrapes bgm.tv search results and saves them to the
/// cache (bangumi_info_easy).  The search page scrape is lightweight; the
/// heavy detail scrape is cached via [`get_bangumi`].
pub async fn search_bangumi(
    State(pool): State<MySqlPool>,
    Query(params): Query<SearchQuery>,
) -> (StatusCode, Json<ApiResponse<Vec<BangumiSearchItem>>>) {
    let title = match &params.q {
        Some(t) if !t.trim().is_empty() => t.trim().to_string(),
        _ => return bad_request("Missing search query"),
    };

    let v1_resp = v1_search_title(
        State(pool.clone()),
        Json(TitleSearchQuery {
            title: Some(title),
            page: params.page,
        }),
    )
    .await;

    let inner = v1_resp.0;
    if inner.status == 0 {
        success(inner.data.unwrap_or_default())
    } else {
        internal_error("Search failed")
    }
}

/// GET /api/v2/bangumi/:id?force=true
///
/// Returns cached result from bangumi_info_detailed if the data was last
/// updated within 24 hours, avoiding a scrape of bgm.tv.  Pass `?force=true`
/// to skip the cache and force a fresh scrape.
pub async fn get_bangumi(
    State(pool): State<MySqlPool>,
    Path(id): Path<u32>,
    Query(query): Query<BangumiQuery>,
) -> (StatusCode, Json<ApiResponse<BangumiItem>>) {
    let force = query.force.unwrap_or(false);
    let id_str = id.to_string();

    if !force
        && let Ok(Some(row)) = sqlx::query!(
            r#"
            SELECT b.external_id, b.title, b.cover_url, b.type              AS "type?: i8",
                   d.author, d.release_date, d.episodes, d.description,
                   d.updated_at                                             AS "updated_at?"
            FROM bangumi_info_easy b
            LEFT JOIN bangumi_info_detailed d ON d.bangumi_id = b.id
            WHERE b.external_id = ?
            "#,
            id_str
        )
        .fetch_optional(&pool)
        .await
        && let Some(ts) = row.updated_at
        && Utc::now().naive_utc() <= ts + Duration::hours(24)
    {
        return success(BangumiItem {
            source: "bangumi".to_string(),
            bangumi_id: row.external_id,
            title: row.title,
            cover_url: row.cover_url.unwrap_or_default(),
            r#type: row.r#type.unwrap_or(8),
            author: row.author.unwrap_or_default(),
            release_date: row.release_date,
            episodes: row.episodes.unwrap_or(0),
            description: row.description.unwrap_or_default(),
        });
    }

    let v1_resp = v1_search_by_id(
        State(pool.clone()),
        Json(IDSearchQuery {
            id: Some(id),
            force,
        }),
    )
    .await;

    let inner = v1_resp.0;
    match inner.status {
        0 => match inner.data {
            Some(item) => success(item),
            None => not_found("Bangumi not found"),
        },
        _ => internal_error("Search failed"),
    }
}

/// POST /api/v2/bangumi/:id/refresh
///
/// Re-fetches the Bangumi subject and explicitly replaces cacheable content.
pub async fn refresh_bangumi(
    State(pool): State<MySqlPool>,
    Path(id): Path<u32>,
) -> (StatusCode, Json<ApiResponse<BangumiItem>>) {
    let v1_resp = v1_search_by_id(
        State(pool),
        Json(IDSearchQuery {
            id: Some(id),
            force: true,
        }),
    )
    .await;
    let inner = v1_resp.0;
    match inner.status {
        0 => match inner.data {
            Some(item) => success(item),
            None => not_found("Bangumi not found"),
        },
        -3 => not_found("Bangumi not found"),
        _ => internal_error("Unable to refresh Bangumi content"),
    }
}

/// GET /api/v2/imdb/search?q=keyword&page=1&use_api=false
pub async fn search_imdb(
    State(pool): State<MySqlPool>,
    Query(params): Query<ImdbSearchParams>,
) -> (StatusCode, Json<ApiResponse<Vec<ImdbSearchItem>>>) {
    let title = match &params.q {
        Some(t) if !t.trim().is_empty() => t.trim().to_string(),
        _ => return bad_request("Missing search query"),
    };

    let v1_resp = v1_search_imdb(
        State(pool.clone()),
        Json(ImdbSearchQuery {
            q: Some(title),
            page: params.page,
            use_api: params.use_api,
        }),
    )
    .await;

    let inner = v1_resp.0;
    if inner.status == 0 {
        success(inner.data.unwrap_or_default())
    } else {
        internal_error("IMDb search failed")
    }
}

/// GET /api/v2/imdb/:id?force=true&use_api=false
pub async fn get_imdb(
    State(pool): State<MySqlPool>,
    Path(id): Path<String>,
    Query(query): Query<ImdbQuery>,
) -> (StatusCode, Json<ApiResponse<ImdbItem>>) {
    let v1_resp = v1_search_imdb_by_id(
        State(pool.clone()),
        Json(ImdbIDSearchQuery {
            id: Some(id),
            force: query.force,
            use_api: query.use_api,
        }),
    )
    .await;

    let inner = v1_resp.0;
    match inner.status {
        0 => match inner.data {
            Some(item) => success(item),
            None => not_found("IMDb title not found"),
        },
        -1 => bad_request("Invalid IMDb id"),
        _ => internal_error("IMDb detail lookup failed"),
    }
}

/// GET /api/v2/other/:id
pub async fn get_other(
    State(pool): State<MySqlPool>,
    Extension(auth_user): Extension<AuthUser>,
    Path(id): Path<u32>,
) -> (StatusCode, Json<ApiResponse<OtherItem>>) {
    let v1_resp = v1_get_other_by_id(
        State(pool.clone()),
        Extension(auth_user),
        Json(IDSearchQuery {
            id: Some(id),
            force: false,
        }),
    )
    .await;
    let inner = v1_resp.0;
    match inner.status {
        0 => match inner.data {
            Some(item) => success(item),
            None => not_found("Custom item not found"),
        },
        -1 => bad_request("Invalid custom item id"),
        -2 => not_found("Custom item not found"),
        _ => internal_error("Custom item detail lookup failed"),
    }
}

#[derive(Serialize)]
pub struct BangumiEpisodeMeta {
    pub ordinal: i32,
    pub title: Option<String>,
    pub name_cn: Option<String>,
    pub airdate: Option<NaiveDate>,
    pub duration: Option<String>,
}

/// GET /api/v2/bangumi/:id/episodes
///
/// Returns the episode list for a bangumi subject, scraped from bgm.tv and
/// cached in the bangumi_episodes table.
pub async fn get_bangumi_episodes(
    State(pool): State<MySqlPool>,
    Path(id): Path<u32>,
    Query(query): Query<EpisodesForceQuery>,
) -> (StatusCode, Json<ApiResponse<Vec<BangumiEpisodeMeta>>>) {
    let id_str = id.to_string();
    let force = query.force.unwrap_or(false);

    let easy_id = match sqlx::query_scalar!(
        "SELECT id FROM bangumi_info_easy WHERE external_id = ?",
        id_str
    )
    .fetch_optional(&pool)
    .await
    {
        Ok(Some(eid)) => eid,
        Ok(None) => return not_found("Bangumi not found in cache. Search first."),
        Err(e) => {
            log::error!("DB error: {:?}", e);
            return internal_error("Database error");
        }
    };

    if let Err(error) = ensure_episode_metadata_cached(&pool, easy_id, &id_str, force).await {
        log::warn!(
            "Unable to refresh episode metadata for Bangumi {}: {}",
            id,
            error
        );
    }

    let rows = sqlx::query_as::<
        _,
        (
            i32,
            Option<String>,
            Option<String>,
            Option<NaiveDate>,
            Option<String>,
        ),
    >(
        r#"
        SELECT ordinal, title, name_cn, airdate, duration
        FROM bangumi_episodes
        WHERE bangumi_easy_id = ?
        ORDER BY ordinal ASC
        "#,
    )
    .bind(easy_id)
    .fetch_all(&pool)
    .await;

    match rows {
        Ok(rows) => {
            let episodes: Vec<BangumiEpisodeMeta> = rows
                .into_iter()
                .map(|r| BangumiEpisodeMeta {
                    ordinal: r.0,
                    title: r.1,
                    name_cn: r.2,
                    airdate: r.3,
                    duration: r.4,
                })
                .collect();
            success(episodes)
        }
        Err(e) => {
            log::error!("DB error: {:?}", e);
            internal_error("Database error")
        }
    }
}

/// GET /api/v2/search/local?q=keyword&page=1&page_size=20
pub async fn search_local(
    State(pool): State<MySqlPool>,
    Query(params): Query<LocalSearchParams>,
) -> (StatusCode, Json<ApiResponse<LocalSearchResult>>) {
    let v1_resp = v1_search_local(
        State(pool.clone()),
        Json(crate::api::search::LocalSearchQuery {
            keyword: params.q,
            id: params.id,
            page: params.page,
            page_size: params.page_size,
        }),
    )
    .await;

    let inner = v1_resp.0;
    if inner.status == 0 {
        success(LocalSearchResult {
            items: inner.data.unwrap_or_default(),
            total: inner.total.unwrap_or(0),
            page: inner.page.unwrap_or(1),
            page_size: inner.page_size.unwrap_or(20),
        })
    } else {
        internal_error("Search failed")
    }
}
