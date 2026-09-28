use std::{collections::HashMap, sync::OnceLock};

use axum::{
    Json,
    extract::{Extension, State},
};
use serde::{Deserialize, Serialize};

use reqwest::Client;
use scraper::{ElementRef, Html, Selector};
use sqlx::QueryBuilder;
use sqlx::mysql::MySqlPool;
use urlencoding::encode;

use crate::auth_bearer::AuthUser;
use chrono::{Duration, NaiveDate, Utc};

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

#[derive(Debug, Serialize, Deserialize)]
pub struct TitleSearchQuery {
    pub title: Option<String>,
    pub page: Option<i32>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IDSearchQuery {
    pub id: Option<u32>,
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Serialize)]
pub struct IDSearchResponse {
    pub status: i32,
    pub data: Option<BangumiItem>,
}

#[derive(Debug, Serialize)]
pub struct BangumiItem {
    pub source: String,
    pub bangumi_id: String,
    pub title: String,
    pub cover_url: String,
    pub r#type: i8,
    pub author: String,
    pub release_date: Option<NaiveDate>,
    pub episodes: i32,
    pub description: String,
}

#[derive(Debug, Serialize)]
pub struct BangumiSearchItem {
    pub source: String,
    pub bangumi_id: String,
    pub title: String,
    pub alias: String,
    pub cover: String,
    pub info: String,
    pub r#type: i32,
}

#[derive(Debug, Serialize)]
pub struct SearchBangumiResponse {
    pub status: i32,
    pub data: Option<Vec<BangumiSearchItem>>,
}

fn parse_bangumi_type_easy(item: ElementRef) -> i32 {
    static TYPE_SEL: OnceLock<Selector> = OnceLock::new();
    let type_sel = selector("span.ico_subject_type", &TYPE_SEL);

    let span = match item.select(type_sel).next() {
        Some(s) => s,
        None => return 8, // Other
    };

    let class = span.value().attr("class").unwrap_or_default();

    match () {
        _ if class.contains("subject_type_2") => 1,  // TV
        _ if class.contains("subject_type_4") => 9,  // Game
        _ if class.contains("subject_type_1") => 7,  // Book
        _ if class.contains("subject_type_3") => 6,  // Music
        _ if class.contains("subject_type_6") => 10, // Real
        _ => 8,                                      // Other
    }
}

pub async fn search_bangumi_by_title(
    State(pool): State<MySqlPool>,
    Json(params): Json<TitleSearchQuery>,
) -> Json<SearchBangumiResponse> {
    let title = match &params.title {
        Some(t) if !t.trim().is_empty() => t.trim(),
        _ => {
            return Json(SearchBangumiResponse {
                status: -3,
                data: None,
            });
        }
    };

    let page = params.page.unwrap_or(1);

    let encoded_title = encode(title);

    let url = if page == 1 {
        format!("https://bgm.tv/subject_search/{}?cat=all", encoded_title)
    } else {
        format!(
            "https://bgm.tv/subject_search/{}?cat=all&page={}",
            encoded_title, page
        )
    };

    let resp = match http_client().get(&url).send().await {
        Ok(r) => r,
        Err(_) => {
            return Json(SearchBangumiResponse {
                status: -1,
                data: None,
            });
        }
    };

    let html = match resp.text().await {
        Ok(t) => t,
        Err(_) => {
            return Json(SearchBangumiResponse {
                status: -2,
                data: None,
            });
        }
    };

    let results = {
        let document = Html::parse_document(&html);

        static ITEM_SEL: OnceLock<Selector> = OnceLock::new();
        static TITLE_SEL: OnceLock<Selector> = OnceLock::new();
        static ALIAS_SEL: OnceLock<Selector> = OnceLock::new();
        static COVER_SEL: OnceLock<Selector> = OnceLock::new();
        static INFO_SEL: OnceLock<Selector> = OnceLock::new();

        let item_sel = selector("li.item", &ITEM_SEL);
        let title_sel = selector("h3 a.l", &TITLE_SEL);
        let alias_sel = selector("h3 small", &ALIAS_SEL);
        let cover_sel = selector("img.cover", &COVER_SEL);
        let info_sel = selector("p.info.tip", &INFO_SEL);

        let mut results: Vec<BangumiSearchItem> = Vec::new();

        for item in document.select(item_sel) {
            let a = match item.select(title_sel).next() {
                Some(a) => a,
                None => continue,
            };

            let title = a.text().collect::<String>().trim().to_string();

            let href = match a.value().attr("href") {
                Some(h) => h,
                None => continue,
            };

            let bangumi_id = match href.strip_prefix("/subject/") {
                Some(id) => id.to_string(),
                None => continue,
            };

            let alias = item
                .select(alias_sel)
                .next()
                .map(|e| e.text().collect::<String>().trim().to_string())
                .unwrap_or_default();

            let cover = item
                .select(cover_sel)
                .next()
                .and_then(|img| img.value().attr("src"))
                .map(|src| {
                    if src.starts_with("//") {
                        format!("https:{}", src)
                    } else {
                        src.to_string()
                    }
                })
                .unwrap_or_default();

            let info = item
                .select(info_sel)
                .next()
                .map(|e| {
                    e.text()
                        .collect::<String>()
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();

            let r#type = parse_bangumi_type_easy(item);
            // ===== push =====
            results.push(BangumiSearchItem {
                source: "bangumi".to_string(),
                bangumi_id,
                title,
                alias,
                cover,
                info,
                r#type,
            });
        }
        results
    };

    // Batch upsert all search results into bangumi_info_easy cache
    if !results.is_empty() {
        let mut qb = QueryBuilder::new(
            "INSERT INTO bangumi_info_easy (external_id, title, type, info, cover_url) ",
        );
        qb.push_values(results.iter(), |mut b, r| {
            b.push_bind(r.bangumi_id.as_str())
                .push_bind(r.title.as_str())
                .push_bind(r.r#type)
                .push_bind(r.info.as_str())
                .push_bind(r.cover.as_str());
        });
        qb.push(
            " ON DUPLICATE KEY UPDATE title = VALUES(title), type = VALUES(type), info = VALUES(info), cover_url = VALUES(cover_url), updated_at = CURRENT_TIMESTAMP",
        );
        if let Err(e) = qb.build().execute(&pool).await {
            log::error!("batch upsert bangumi_info_easy error: {:?}", e);
        }
    }

    Json(SearchBangumiResponse {
        status: 0,
        data: Some(results),
    })
}

fn parse_date(input: &str) -> Option<NaiveDate> {
    let cleaned = input
        .replace("年", "-")
        .replace("月", "-")
        .replace("日", "");

    NaiveDate::parse_from_str(&cleaned, "%Y-%m-%d").ok()
}

pub async fn search_bangumi_by_id(
    State(pool): State<MySqlPool>,
    Json(params): Json<IDSearchQuery>,
) -> Json<IDSearchResponse> {
    if params.id.is_none() {
        return Json(IDSearchResponse {
            status: -1,
            data: None,
        });
    }

    let force = params.force;
    let id = params.id.unwrap();

    let _local_bangumi_id: Option<u32> =
        match sqlx::query_scalar!("SELECT id FROM bangumi_info_easy WHERE external_id = ?", id)
            .fetch_optional(&pool)
            .await
        {
            Ok(Some(id)) => Some(id),
            Ok(None) => None,
            Err(_) => {
                return Json(IDSearchResponse {
                    status: -2,
                    data: None,
                });
            }
        };

    if !force && _local_bangumi_id.is_some() {
        match sqlx::query!(
            r#"
            SELECT 
                b.id,
                b.external_id AS bangumi_id,
                b.title,
                b.type,
                d.author,
                d.release_date,
                d.episodes,
                d.description,
                b.cover_url,
                d.updated_at AS "updated_at?"
            FROM bangumi_info_easy b
            LEFT JOIN bangumi_info_detailed d
                ON d.bangumi_id = b.id
            WHERE b.id = ?
            "#,
            _local_bangumi_id.unwrap()
        )
        .fetch_optional(&pool)
        .await
        {
            Ok(Some(r)) => {
                if let Some(updated_at) = r.updated_at
                    && Utc::now().naive_utc() <= updated_at + Duration::hours(24)
                {
                    return Json(IDSearchResponse {
                        status: 0,
                        data: Some(BangumiItem {
                            source: "bangumi".to_string(),
                            bangumi_id: r.bangumi_id,
                            title: r.title,
                            cover_url: r.cover_url.unwrap_or_default(),
                            r#type: r.r#type,
                            author: r.author.unwrap_or_default(),
                            release_date: r.release_date,
                            episodes: r.episodes.unwrap_or(0),
                            description: r.description.unwrap_or_default(),
                        }),
                    });
                }
            }
            Ok(None) => {}
            Err(_) => {}
        }
    }

    let url = format!("https://bgm.tv/subject/{}", id);

    let resp = match http_client().get(&url).send().await {
        Ok(r) => r,
        Err(_) => {
            return Json(IDSearchResponse {
                status: -1,
                data: None,
            });
        }
    };

    // A 404 page has enough HTML for the scraper to produce an empty item.
    // Do not cache that as a real Bangumi subject.
    if !resp.status().is_success() {
        return Json(IDSearchResponse {
            status: -3,
            data: None,
        });
    }

    let html = match resp.text().await {
        Ok(t) => t,
        Err(_) => {
            return Json(IDSearchResponse {
                status: -2,
                data: None,
            });
        }
    };
    let mut result = {
        let document = Html::parse_document(&html);

        static TYPE_SELECTOR: OnceLock<Selector> = OnceLock::new();
        let type_selector = selector("h1.nameSingle small.grey", &TYPE_SELECTOR);

        let type_text = document
            .select(type_selector)
            .next()
            .map(|e| e.text().collect::<String>().trim().to_string())
            .unwrap_or_default();

        let r#type = match type_text.as_str() {
            "TV" => 1,
            "剧场版" => 2,
            "OVA" => 3,
            "ONA" => 4,
            "TV SP" => 5,
            "Music" => 6,
            "书籍" => 7,
            "游戏" | "Game" => 9,
            "三次元" | "电视剧" | "Real" => 10,
            _ => 8,
        };

        static LI_SELECTOR: OnceLock<Selector> = OnceLock::new();
        static SPAN_SELECTOR: OnceLock<Selector> = OnceLock::new();
        let li_selector = selector("#infobox li", &LI_SELECTOR);
        let span_selector = selector("span.tip", &SPAN_SELECTOR);

        let mut info_map: HashMap<String, String> = HashMap::new();

        for li in document.select(li_selector) {
            let key = li.select(span_selector).next().map(|e| {
                e.text()
                    .collect::<String>()
                    .replace(":", "")
                    .trim()
                    .to_string()
            });

            let value = li
                .text()
                .collect::<String>()
                .split(':')
                .skip(1)
                .collect::<String>()
                .trim()
                .to_string();

            if let Some(k) = key {
                info_map.insert(k, value);
            }
        }

        let author = info_map
            .get("原作")
            .or_else(|| info_map.get("作者"))
            .or_else(|| info_map.get("出版社"))
            .or_else(|| info_map.get("开发"))
            .or_else(|| info_map.get("开发商"))
            .or_else(|| info_map.get("发行商"))
            .or_else(|| info_map.get("发行"))
            .or_else(|| info_map.get("制作"))
            .cloned()
            .unwrap_or_default();

        let episodes = info_map
            .get("话数")
            .or_else(|| info_map.get("册数"))
            .or_else(|| info_map.get("卷数"))
            .or_else(|| info_map.get("章节数"))
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(0);

        let release_date = info_map
            .get("放送开始")
            .or_else(|| info_map.get("发售日"))
            .or_else(|| info_map.get("发行日期"))
            .or_else(|| info_map.get("出版日期"))
            .or_else(|| info_map.get("连载开始"))
            .or_else(|| info_map.get("上映年度"))
            .and_then(|v| parse_date(v));

        static DESC_SELECTOR: OnceLock<Selector> = OnceLock::new();
        let desc_selector = selector("#subject_summary", &DESC_SELECTOR);

        let description = document
            .select(desc_selector)
            .next()
            .map(|e| e.text().collect::<String>().trim().to_string())
            .unwrap_or_default();

        let title = document
            .select({
                static THICKBOX_SELECTOR: OnceLock<Selector> = OnceLock::new();
                selector(".infobox a.thickbox", &THICKBOX_SELECTOR)
            })
            .next()
            .and_then(|a| a.value().attr("title"))
            .map(|t| t.trim().to_string())
            .or_else(|| {
                static NAME_SEL: OnceLock<Selector> = OnceLock::new();
                let name_sel = selector("h1.nameSingle", &NAME_SEL);
                document
                    .select(name_sel)
                    .next()
                    .map(|e| e.text().collect::<String>().trim().to_string())
            })
            .unwrap_or_default();

        static COVER_SEL: OnceLock<Selector> = OnceLock::new();
        let cover_sel = selector(".infobox img.cover", &COVER_SEL);
        let cover_url = document
            .select(cover_sel)
            .next()
            .and_then(|img| img.value().attr("src"))
            .map(|src| {
                if src.starts_with("//") {
                    format!("https:{}", src)
                } else {
                    src.to_string()
                }
            })
            .unwrap_or_default();

        BangumiItem {
            source: "bangumi".to_string(),
            bangumi_id: id.to_string(),
            title,
            cover_url,
            r#type,
            author,
            release_date,
            episodes,
            description,
        }
    };

    // Keep invalid/error pages out of the cache even when they happened to
    // return HTTP 200 (for example, an upstream anti-bot response).
    if result.title.trim().is_empty() {
        return Json(IDSearchResponse {
            status: -3,
            data: None,
        });
    }

    let bangumi_easy_id: u32 = match sqlx::query!(
        r#"SELECT id, title, cover_url FROM bangumi_info_easy WHERE external_id = ?"#,
        id
    )
    .fetch_optional(&pool)
    .await
    {
        Ok(Some(row)) => {
            if force {
                if sqlx::query(
                    "UPDATE bangumi_info_easy SET title = ?, type = ?, cover_url = ? WHERE id = ?",
                )
                .bind(&result.title)
                .bind(result.r#type)
                .bind(&result.cover_url)
                .bind(row.id)
                .execute(&pool)
                .await
                .is_err()
                {
                    return Json(IDSearchResponse {
                        status: -4,
                        data: None,
                    });
                }
            } else {
                if !row.title.is_empty() {
                    result.title = row.title;
                }
                if let Some(ref db_cover) = row.cover_url
                    && !db_cover.is_empty()
                {
                    result.cover_url = db_cover.clone();
                }
            }
            row.id
        }
        Ok(None) => {
            let insert_result = sqlx::query!(
                r#"
                INSERT INTO bangumi_info_easy
                (external_id, title, type, info, cover_url)
                VALUES (?, ?, ?, ?, ?)
                "#,
                id,
                result.title,
                result.r#type,
                "",
                result.cover_url
            )
            .execute(&pool)
            .await;

            match insert_result {
                Ok(res) => res.last_insert_id() as u32,
                Err(_) => {
                    return Json(IDSearchResponse {
                        status: -4,
                        data: None,
                    });
                }
            }
        }
        Err(_) => {
            return Json(IDSearchResponse {
                status: -5,
                data: None,
            });
        }
    };

    match sqlx::query_scalar!(
        "SELECT id FROM bangumi_info_detailed WHERE bangumi_id = ?",
        bangumi_easy_id
    )
    .fetch_optional(&pool)
    .await
    {
        Ok(Some(_)) => {
            if let Err(e) = sqlx::query!(
                "UPDATE bangumi_info_detailed SET type = ?, author = ?, release_date = ?, episodes = ?, description = ?, updated_at = CURRENT_TIMESTAMP WHERE bangumi_id = ?",
                result.r#type,
                result.author,
                result.release_date,
                result.episodes,
                result.description,
                bangumi_easy_id,
            )
            .execute(&pool)
            .await
            {
                log::error!("Failed to update bangumi_info_detailed: {:?}", e);
            }
        }
        Ok(None) => {
            if let Err(e) = sqlx::query!(
                "INSERT INTO bangumi_info_detailed (bangumi_id, type, author, release_date, episodes, description) VALUES (?, ?, ?, ?, ?, ?)",
                bangumi_easy_id,
                result.r#type,
                result.author,
                result.release_date,
                result.episodes,
                result.description,
            )
            .execute(&pool)
            .await
            {
                log::error!("Failed to insert bangumi_info_detailed: {:?}", e);
            }
        }
        Err(e) => {
            log::error!("Failed to query bangumi_info_detailed: {:?}", e);
        }
    };

    Json(IDSearchResponse {
        status: 0,
        data: Some(result),
    })
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LocalSearchQuery {
    pub keyword: Option<String>,
    pub id: Option<u32>,
    pub page: Option<i32>,
    pub page_size: Option<i32>,
}

#[derive(Debug, Serialize, Clone)]
pub struct LocalSearchItem {
    pub source: String,
    pub bangumi_id: Option<String>,
    pub imdb_id: Option<String>,
    pub other_id: Option<u32>,
    pub title: String,
    pub cover: Option<String>,
    pub info: Option<String>,
    pub r#type: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct OtherItem {
    pub source: String,
    pub other_id: u32,
    pub title: String,
    pub cover_url: String,
    pub r#type: i8,
    pub author: String,
    pub release_date: Option<NaiveDate>,
    pub episodes: i32,
    pub description: String,
    pub status: Option<i8>,
}

fn media_type_label(source: &str, media_type: i8) -> String {
    match (source, media_type) {
        ("imdb", 1) => "IMDb TV".into(),
        ("imdb", 2) => "IMDb Movie".into(),
        ("imdb", 9) => "IMDb Game".into(),
        (_, 1) => "TV".into(),
        (_, 2) => "剧场版".into(),
        (_, 3) => "OVA".into(),
        (_, 4) => "ONA".into(),
        (_, 5) => "TV SP".into(),
        (_, 6) => "Music".into(),
        (_, 7) => "书籍".into(),
        (_, 9) => "游戏".into(),
        (_, 10) => "三次元".into(),
        _ => "其他".into(),
    }
}

#[derive(Debug, Serialize)]
pub struct LocalSearchResponse {
    pub status: i32,
    pub data: Option<Vec<LocalSearchItem>>,
    pub total: Option<i64>,
    pub page: Option<i32>,
    pub page_size: Option<i32>,
}

#[derive(Debug, Serialize)]
pub struct OtherDetailResponse {
    pub status: i32,
    pub data: Option<OtherItem>,
}

pub async fn get_other_by_id(
    State(pool): State<MySqlPool>,
    Extension(auth_user): Extension<AuthUser>,
    Json(params): Json<IDSearchQuery>,
) -> Json<OtherDetailResponse> {
    let id = match params.id {
        Some(id) => id,
        None => {
            return Json(OtherDetailResponse {
                status: -1,
                data: None,
            });
        }
    };

    #[derive(sqlx::FromRow)]
    struct OtherRow {
        id: u32,
        name: Option<String>,
        description: Option<String>,
        cover_url: Option<String>,
        max_number: Option<i32>,
        status: Option<i8>,
    }

    match sqlx::query_as::<_, OtherRow>(
        "SELECT id, name, description, cover_url, max_number, status FROM other_recorders WHERE id = ? AND (add_user IS NULL OR add_user = ?)",
    )
    .bind(id)
    .bind(auth_user.user_id)
    .fetch_optional(&pool)
    .await
    {
        Ok(Some(r)) => Json(OtherDetailResponse {
            status: 0,
            data: Some(OtherItem {
                source: "custom".to_string(),
                other_id: r.id,
                title: r.name.unwrap_or_else(|| "未命名条目".to_string()),
                cover_url: r.cover_url.unwrap_or_default(),
                r#type: 8,
                author: String::new(),
                release_date: None,
                episodes: r.max_number.unwrap_or(0),
                description: r.description.unwrap_or_default(),
                status: r.status,
            }),
        }),
        Ok(None) => Json(OtherDetailResponse {
            status: -2,
            data: None,
        }),
        Err(e) => {
            log::error!("Failed to query custom item {}: {:?}", id, e);
            Json(OtherDetailResponse {
                status: -3,
                data: None,
            })
        }
    }
}

pub async fn search_local(
    State(pool): State<MySqlPool>,
    Json(params): Json<LocalSearchQuery>,
) -> Json<LocalSearchResponse> {
    let mut results: Vec<LocalSearchItem> = Vec::new();

    if let Some(id) = params.id {
        let bangumi = sqlx::query!(
            "SELECT id, external_id, title, type, cover_url FROM bangumi_info_easy WHERE external_id = ?",
            id
        )
        .fetch_optional(&pool)
        .await;

        if let Ok(Some(r)) = bangumi {
            results.push(LocalSearchItem {
                source: "bangumi".into(),
                bangumi_id: Some(r.external_id),
                imdb_id: None,
                other_id: None,
                title: r.title,
                cover: r.cover_url,
                info: Some(format!("ID: {} · 本地缓存", r.id)),
                r#type: Some(media_type_label("bangumi", r.r#type)),
            });
        }

        let imdb = sqlx::query!(
            "SELECT id, external_id, title, type, cover_url FROM external_media WHERE source = 'imdb' AND id = ?",
            id
        )
        .fetch_optional(&pool)
        .await;

        if let Ok(Some(r)) = imdb {
            results.push(LocalSearchItem {
                source: "imdb".into(),
                bangumi_id: None,
                imdb_id: Some(r.external_id),
                other_id: None,
                title: r.title,
                cover: r.cover_url,
                info: Some(format!("ID: {} · IMDb 本地缓存", r.id)),
                r#type: Some(media_type_label("imdb", r.r#type)),
            });
        }

        let other = sqlx::query!(
            "SELECT id, name, description, cover_url FROM other_recorders WHERE id = ? AND add_user IS NULL",
            id
        )
        .fetch_optional(&pool)
        .await;

        if let Ok(Some(r)) = other {
            results.push(LocalSearchItem {
                source: "custom".into(),
                bangumi_id: None,
                imdb_id: None,
                other_id: Some(r.id),
                title: r.name.unwrap_or_else(|| "未命名条目".into()),
                cover: r.cover_url,
                info: r.description,
                r#type: Some("自定义".into()),
            });
        }

        let count = results.len() as i64;
        return Json(LocalSearchResponse {
            status: 0,
            data: Some(results),
            total: Some(count),
            page: Some(1),
            page_size: Some(20),
        });
    }

    if let Some(keyword) = &params.keyword {
        let keyword = keyword.trim();
        if !keyword.is_empty() {
            let page = params.page.unwrap_or(1).max(1);
            let page_size = params.page_size.unwrap_or(20).clamp(1, 100);
            let like_pattern = format!("%{}%", keyword);

            let bangumi_count = sqlx::query_scalar!(
                "SELECT COUNT(*) as cnt FROM bangumi_info_easy WHERE title LIKE ?",
                like_pattern
            )
            .fetch_one(&pool)
            .await
            .unwrap_or(0) as i64;

            let imdb_count = sqlx::query_scalar!(
                "SELECT COUNT(*) as cnt FROM external_media WHERE source = 'imdb' AND title LIKE ?",
                like_pattern
            )
            .fetch_one(&pool)
            .await
            .unwrap_or(0) as i64;

            let other_count = sqlx::query_scalar!(
                "SELECT COUNT(*) as cnt FROM other_recorders WHERE name LIKE ? AND add_user IS NULL",
                like_pattern
            )
            .fetch_one(&pool)
            .await
            .unwrap_or(0) as i64;

            let total = bangumi_count + imdb_count + other_count;

            let offset = (page - 1) * page_size;
            let bangumi_rows = sqlx::query!(
                "SELECT id, external_id, title, type, cover_url FROM bangumi_info_easy WHERE title LIKE ? ORDER BY id LIMIT ? OFFSET ?",
                like_pattern,
                page_size as i64,
                offset as i64
            )
            .fetch_all(&pool)
            .await;

            if let Ok(rows) = bangumi_rows {
                for r in rows {
                    results.push(LocalSearchItem {
                        source: "bangumi".into(),
                        bangumi_id: Some(r.external_id),
                        imdb_id: None,
                        other_id: None,
                        title: r.title,
                        cover: r.cover_url,
                        info: Some(format!("ID: {} · 本地缓存", r.id)),
                        r#type: Some(media_type_label("bangumi", r.r#type)),
                    });
                }
            }

            let imdb_rows = sqlx::query!(
                "SELECT id, external_id, title, type, cover_url FROM external_media WHERE source = 'imdb' AND title LIKE ? ORDER BY id LIMIT ? OFFSET ?",
                like_pattern,
                page_size as i64,
                offset as i64
            )
            .fetch_all(&pool)
            .await;

            if let Ok(rows) = imdb_rows {
                for r in rows {
                    results.push(LocalSearchItem {
                        source: "imdb".into(),
                        bangumi_id: None,
                        imdb_id: Some(r.external_id),
                        other_id: None,
                        title: r.title,
                        cover: r.cover_url,
                        info: Some(format!("ID: {} · IMDb 本地缓存", r.id)),
                        r#type: Some(media_type_label("imdb", r.r#type)),
                    });
                }
            }

            let other_rows = sqlx::query!(
                "SELECT id, name, description, cover_url FROM other_recorders WHERE name LIKE ? AND add_user IS NULL ORDER BY id LIMIT ? OFFSET ?",
                like_pattern,
                page_size as i64,
                offset as i64
            )
            .fetch_all(&pool)
            .await;

            if let Ok(rows) = other_rows {
                for r in rows {
                    results.push(LocalSearchItem {
                        source: "custom".into(),
                        bangumi_id: None,
                        imdb_id: None,
                        other_id: Some(r.id),
                        title: r.name.unwrap_or_else(|| "未命名条目".into()),
                        cover: r.cover_url,
                        info: r.description,
                        r#type: Some("自定义".into()),
                    });
                }
            }

            return Json(LocalSearchResponse {
                status: 0,
                data: Some(results),
                total: Some(total),
                page: Some(page),
                page_size: Some(page_size),
            });
        }
    }

    Json(LocalSearchResponse {
        status: 0,
        data: Some(results),
        total: Some(0),
        page: Some(1),
        page_size: Some(20),
    })
}
