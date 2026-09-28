use axum::{
    Json,
    extract::{Extension, State},
};
use chrono::NaiveDateTime;
use serde::Serialize;
use sqlx::mysql::MySqlPool;

use crate::auth_bearer::AuthUser;

#[derive(Serialize)]
pub struct DetailListResponse {
    pub status: i32,
    pub data: Option<Vec<DetailListItem>>,
}

#[derive(Serialize)]
pub struct DetailListItem {
    pub id: u32,
    pub source: Option<String>,
    pub external_id: Option<String>,
    pub local_external_media_id: Option<u32>,
    pub local_bangumi_id: Option<u32>,
    pub other_id: Option<u32>,
    pub bangumi_id: Option<String>,
    pub imdb_id: Option<String>,
    pub title: Option<String>,
    pub r#type: Option<i8>,
    pub author: Option<String>,
    pub episodes: i32,
    pub cover_url: Option<String>,
    pub recorder: Option<String>,
    pub user_status: Option<i8>,
    pub is_delete: bool,
    pub updated_at: NaiveDateTime,
    pub created_at: NaiveDateTime,
}

pub async fn get_detail_list(
    State(pool): State<MySqlPool>,
    Extension(auth_user): Extension<AuthUser>,
) -> Json<DetailListResponse> {
    let mut results: Vec<DetailListItem> = Vec::new();

    match sqlx::query!(
        r#"
        SELECT 
            r.id,
            r.bangumi_id AS local_bangumi_id,
            b.external_id AS external_id,
            b.title,
            b.type,
            d.author,
            d.episodes,
            b.cover_url,
            r.recorder,
            r.status,
            r.is_delete,
            r.updated_at,
            r.created_at
        FROM recordings r
        LEFT JOIN bangumi_info_easy b
            ON r.bangumi_id = b.id
        LEFT JOIN bangumi_info_detailed d
            ON d.bangumi_id = b.id
        WHERE r.user_id = ? AND r.bangumi_id IS NOT NULL
        "#,
        auth_user.user_id
    )
    .fetch_all(&pool)
    .await
    {
        Ok(rows) => {
            for r in rows {
                results.push(DetailListItem {
                    id: r.id,
                    source: Some("bangumi".to_string()),
                    external_id: r.external_id.clone(),
                    local_external_media_id: None,
                    local_bangumi_id: r.local_bangumi_id,
                    other_id: None,
                    bangumi_id: r.external_id,
                    imdb_id: None,
                    title: r.title,
                    r#type: r.r#type,
                    author: r.author,
                    episodes: r.episodes.unwrap_or(0),
                    cover_url: r.cover_url,
                    recorder: r.recorder,
                    user_status: Some(r.status),
                    is_delete: r.is_delete != 0,
                    updated_at: r.updated_at,
                    created_at: r.created_at,
                });
            }
        }
        Err(e) => {
            log::error!("DB error: {:?}", e);
            return Json(DetailListResponse {
                status: -1,
                data: None,
            });
        }
    }

    match sqlx::query!(
        r#"
        SELECT 
            r.id,
            r.external_media_id AS local_external_media_id,
            e.source,
            e.external_id,
            e.title,
            e.type,
            d.author,
            d.episodes,
            e.cover_url,
            r.recorder,
            r.status,
            r.is_delete,
            r.updated_at,
            r.created_at
        FROM recordings r
        LEFT JOIN external_media e ON r.external_media_id = e.id
        LEFT JOIN external_media_detailed d ON d.media_id = e.id
        WHERE r.user_id = ? AND r.external_media_id IS NOT NULL
        "#,
        auth_user.user_id
    )
    .fetch_all(&pool)
    .await
    {
        Ok(rows) => {
            for r in rows {
                results.push(DetailListItem {
                    id: r.id,
                    source: r.source.clone(),
                    external_id: r.external_id.clone(),
                    local_external_media_id: r.local_external_media_id,
                    local_bangumi_id: None,
                    other_id: None,
                    bangumi_id: None,
                    imdb_id: if r.source.as_deref() == Some("imdb") {
                        r.external_id
                    } else {
                        None
                    },
                    title: r.title,
                    r#type: r.r#type,
                    author: r.author,
                    episodes: r.episodes.unwrap_or(0),
                    cover_url: r.cover_url,
                    recorder: r.recorder,
                    user_status: Some(r.status),
                    is_delete: r.is_delete != 0,
                    updated_at: r.updated_at,
                    created_at: r.created_at,
                });
            }
        }
        Err(e) => {
            log::error!("External media detail list DB error: {:?}", e);
            return Json(DetailListResponse {
                status: -1,
                data: None,
            });
        }
    }

    match sqlx::query!(
        r#"
        SELECT 
            r.id,
            r.bangumi_id AS local_bangumi_id,
            r.external_media_id AS local_external_media_id,
            r.other_id,
            o.name AS title,
            o.cover_url,
            o.max_number AS episodes,
            r.recorder,
            r.status,
            r.is_delete,
            r.updated_at,
            r.created_at
        FROM recordings r
        LEFT JOIN other_recorders o ON r.other_id = o.id
        WHERE r.user_id = ? AND r.other_id IS NOT NULL
        "#,
        auth_user.user_id
    )
    .fetch_all(&pool)
    .await
    {
        Ok(rows) => {
            for r in rows {
                results.push(DetailListItem {
                    id: r.id,
                    source: Some("custom".to_string()),
                    external_id: None,
                    local_external_media_id: r.local_external_media_id,
                    local_bangumi_id: r.local_bangumi_id,
                    other_id: r.other_id,
                    bangumi_id: None,
                    imdb_id: None,
                    title: r.title,
                    r#type: None,
                    author: None,
                    episodes: r.episodes.unwrap_or(0),
                    cover_url: r.cover_url,
                    recorder: r.recorder,
                    user_status: Some(r.status),
                    is_delete: r.is_delete != 0,
                    updated_at: r.updated_at,
                    created_at: r.created_at,
                });
            }
        }
        Err(e) => {
            log::error!("DB error: {:?}", e);
            return Json(DetailListResponse {
                status: -1,
                data: None,
            });
        }
    }

    Json(DetailListResponse {
        status: 0,
        data: Some(results),
    })
}
