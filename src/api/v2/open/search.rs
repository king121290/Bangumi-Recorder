use crate::auth_bearer::AuthUser;
use axum::{
    Json,
    extract::{Extension, Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use serde::{Deserialize, Serialize};
use sqlx::mysql::MySqlPool;

use crate::api::open::api_token::{
    PERM_READ, PERM_WRITE, api_token_from_request, require_token_with_perm,
};
use crate::api::v2::response::{
    ApiResponse, forbidden as v2_forbidden, unauthorized as v2_unauthorized,
};
use crate::api::v2::search::{
    get_bangumi as v2_get_bangumi, get_imdb as v2_get_imdb, get_other as v2_get_other,
    search_bangumi as v2_search_bangumi, search_imdb as v2_search_imdb,
    search_local as v2_search_local,
};

pub use crate::api::search::BangumiItem;
pub use crate::api::v2::search::BangumiEpisodeMeta;
pub use crate::api::v2::search::LocalSearchResult;

#[derive(Deserialize)]
pub struct SearchQuery {
    pub q: Option<String>,
    pub page: Option<i32>,
    pub token: Option<String>,
}

#[derive(Deserialize)]
pub struct ImdbSearchQuery {
    pub q: Option<String>,
    pub page: Option<i32>,
    pub use_api: Option<bool>,
    pub token: Option<String>,
}

#[derive(Deserialize)]
pub struct BangumiQuery {
    pub force: Option<bool>,
    pub token: Option<String>,
}

#[derive(Deserialize)]
pub struct ImdbQuery {
    pub force: Option<bool>,
    pub use_api: Option<bool>,
    pub token: Option<String>,
}

#[derive(Deserialize)]
pub struct LocalSearchParams {
    pub q: Option<String>,
    pub id: Option<u32>,
    pub page: Option<i32>,
    pub page_size: Option<i32>,
    pub token: Option<String>,
}

async fn verify_token<T: Serialize>(
    pool: &MySqlPool,
    token: Option<&str>,
) -> Result<AuthUser, (StatusCode, Json<ApiResponse<T>>)> {
    match require_token_with_perm(pool, token, &[PERM_READ, PERM_WRITE]).await {
        Ok(info) => Ok(AuthUser {
            user_id: info.user_id,
        }),
        Err(StatusCode::UNAUTHORIZED) => Err(v2_unauthorized("Invalid API token")),
        Err(StatusCode::FORBIDDEN) => Err(v2_forbidden("Insufficient permissions")),
        Err(_) => Err(v2_unauthorized("Invalid API token")),
    }
}

/// GET /api/v2/open/search?q=keyword&page=1&force=true&token=xxx
pub async fn search_bangumi(
    State(pool): State<MySqlPool>,
    headers: HeaderMap,
    Query(params): Query<SearchQuery>,
) -> (
    StatusCode,
    Json<ApiResponse<Vec<crate::api::search::BangumiSearchItem>>>,
) {
    if let Err(e) = verify_token(
        &pool,
        api_token_from_request(&headers, params.token.as_deref()),
    )
    .await
    {
        return e;
    }

    v2_search_bangumi(
        State(pool),
        Query(crate::api::v2::search::SearchQuery {
            q: params.q,
            page: params.page,
        }),
    )
    .await
}

/// GET /api/v2/open/imdb/search?q=keyword&page=1&use_api=false&token=xxx
pub async fn search_imdb(
    State(pool): State<MySqlPool>,
    headers: HeaderMap,
    Query(params): Query<ImdbSearchQuery>,
) -> (
    StatusCode,
    Json<ApiResponse<Vec<crate::api::imdb::ImdbSearchItem>>>,
) {
    if let Err(e) = verify_token(
        &pool,
        api_token_from_request(&headers, params.token.as_deref()),
    )
    .await
    {
        return e;
    }

    v2_search_imdb(
        State(pool),
        Query(crate::api::v2::search::ImdbSearchParams {
            q: params.q,
            page: params.page,
            use_api: params.use_api,
        }),
    )
    .await
}

/// GET /api/v2/open/imdb/:id?force=true&use_api=false&token=xxx
pub async fn get_imdb(
    State(pool): State<MySqlPool>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Query(params): Query<ImdbQuery>,
) -> (StatusCode, Json<ApiResponse<crate::api::imdb::ImdbItem>>) {
    if let Err(e) = verify_token(
        &pool,
        api_token_from_request(&headers, params.token.as_deref()),
    )
    .await
    {
        return e;
    }

    v2_get_imdb(
        State(pool),
        Path(id),
        Query(crate::api::v2::search::ImdbQuery {
            force: params.force,
            use_api: params.use_api,
        }),
    )
    .await
}

/// GET /api/v2/open/other/:id?token=xxx
pub async fn get_other(
    State(pool): State<MySqlPool>,
    Path(id): Path<u32>,
    headers: HeaderMap,
    Query(params): Query<BangumiQuery>,
) -> (StatusCode, Json<ApiResponse<crate::api::search::OtherItem>>) {
    let auth_user = match verify_token(
        &pool,
        api_token_from_request(&headers, params.token.as_deref()),
    )
    .await
    {
        Ok(auth_user) => auth_user,
        Err(e) => return e,
    };

    v2_get_other(State(pool), Extension(auth_user), Path(id)).await
}

/// GET /api/v2/open/bangumi/:id?force=true&token=xxx
pub async fn get_bangumi(
    State(pool): State<MySqlPool>,
    Path(id): Path<u32>,
    headers: HeaderMap,
    Query(params): Query<BangumiQuery>,
) -> (StatusCode, Json<ApiResponse<BangumiItem>>) {
    if let Err(e) = verify_token(
        &pool,
        api_token_from_request(&headers, params.token.as_deref()),
    )
    .await
    {
        return e;
    }

    v2_get_bangumi(
        State(pool),
        Path(id),
        Query(crate::api::v2::search::BangumiQuery {
            force: params.force,
        }),
    )
    .await
}

#[derive(Deserialize)]
pub struct EpisodeListQuery {
    pub token: Option<String>,
    pub force: Option<bool>,
}

/// GET /api/v2/open/bangumi/:id/episodes?force=true&token=xxx
pub async fn get_bangumi_episodes(
    State(pool): State<MySqlPool>,
    Path(id): Path<u32>,
    headers: HeaderMap,
    Query(params): Query<EpisodeListQuery>,
) -> (StatusCode, Json<ApiResponse<Vec<BangumiEpisodeMeta>>>) {
    if let Err(e) = verify_token(
        &pool,
        api_token_from_request(&headers, params.token.as_deref()),
    )
    .await
    {
        return e;
    }

    crate::api::v2::search::get_bangumi_episodes(
        State(pool),
        Path(id),
        Query(crate::api::v2::search::EpisodesForceQuery {
            force: params.force,
        }),
    )
    .await
}

/// GET /api/v2/open/search/local?q=keyword&token=xxx
pub async fn search_local(
    State(pool): State<MySqlPool>,
    headers: HeaderMap,
    Query(params): Query<LocalSearchParams>,
) -> (StatusCode, Json<ApiResponse<LocalSearchResult>>) {
    if let Err(e) = verify_token(
        &pool,
        api_token_from_request(&headers, params.token.as_deref()),
    )
    .await
    {
        return e;
    }

    v2_search_local(
        State(pool),
        Query(crate::api::v2::search::LocalSearchParams {
            q: params.q,
            id: params.id,
            page: params.page,
            page_size: params.page_size,
        }),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn other_detail_requires_api_token() {
        let pool = MySqlPool::connect_lazy("mysql://unused:unused@127.0.0.1/unused").unwrap();
        let (status, response) = get_other(
            State(pool),
            Path(1),
            HeaderMap::new(),
            Query(BangumiQuery {
                force: None,
                token: None,
            }),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(response.0.data.is_none());
    }
}
