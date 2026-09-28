use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use serde::{Deserialize, Serialize};
use sqlx::mysql::MySqlPool;

use crate::api::open::delete_recorder::DeleteRecorderQuery;
use crate::api::open::delete_recorder::delete_recorder as v1_delete_recorder;
use crate::api::open::detail_list::DetailListQuery;
use crate::api::open::detail_list::{
    DetailListItem as OpenDetailListItem, get_detail_list as v1_get_detail_list,
};
use crate::api::open::get_recorder::GetRecorderQuery;
use crate::api::open::get_recorder::get_recorder as v1_get_recorder;
use crate::api::open::list::ListRecorderQuery;
use crate::api::open::list::{RecorderItem as OpenRecorderItem, list_recorder as v1_list_recorder};
use crate::api::open::new::AddRecordQuery;
use crate::api::open::new::add_record_open as v1_add_record_open;
use crate::api::open::update_recorder::UpdateRecorderQuery;
use crate::api::open::update_recorder::update_user_recorder as v1_update_recorder;
use crate::api::v2::record::{AddRecordData, GetRecordData};
use crate::api::v2::response::{
    ApiResponse, bad_request, forbidden, internal_error, not_found, success, success_empty,
    unauthorized,
};

#[derive(Deserialize)]
pub struct OpenTokenQuery {
    pub token: Option<String>,
    pub hard_delete: Option<bool>,
}

fn handle_v1_err<T: Serialize>(e: StatusCode) -> (StatusCode, Json<ApiResponse<T>>) {
    match e {
        StatusCode::UNAUTHORIZED => unauthorized("Invalid API token"),
        StatusCode::FORBIDDEN => forbidden("Insufficient permissions"),
        _ => internal_error("Internal server error"),
    }
}

pub async fn add_record(
    State(pool): State<MySqlPool>,
    headers: HeaderMap,
    query: Query<AddRecordQuery>,
) -> (StatusCode, Json<ApiResponse<AddRecordData>>) {
    match v1_add_record_open(State(pool.clone()), headers, query).await {
        Ok(json_resp) => {
            let inner = json_resp.0;
            match inner.status {
                0 => success(AddRecordData {
                    source: inner.source,
                    external_id: inner.external_id,
                    local_external_media_id: inner.local_external_media_id,
                    local_bangumi_id: inner.local_bangumi_id,
                    other_id: inner.other_id,
                    bangumi_id: inner.bangumi_id,
                    imdb_id: inner.imdb_id,
                    recorder: inner.recorder,
                    date: inner.date.map(|d| d.and_hms_opt(0, 0, 0).unwrap()),
                }),
                -2 => not_found("Item not found"),
                -3 => conflict("Record already exists"),
                _ => bad_request("Invalid parameters"),
            }
        }
        Err(e) => handle_v1_err(e),
    }
}

fn conflict<T: Serialize>(msg: &str) -> (StatusCode, Json<ApiResponse<T>>) {
    (
        StatusCode::CONFLICT,
        Json(crate::api::v2::response::ApiResponse {
            status: -1,
            data: None,
            message: Some(msg.to_string()),
        }),
    )
}

pub async fn list_recorder(
    State(pool): State<MySqlPool>,
    headers: HeaderMap,
    query: Query<ListRecorderQuery>,
) -> (StatusCode, Json<ApiResponse<Vec<OpenRecorderItem>>>) {
    match v1_list_recorder(State(pool.clone()), headers, query).await {
        Ok(json_resp) => {
            let inner = json_resp.0;
            if inner.status == 0 {
                success(inner.data.unwrap_or_default())
            } else {
                internal_error("Failed to list records")
            }
        }
        Err(e) => handle_v1_err(e),
    }
}

pub async fn get_detail_list(
    State(pool): State<MySqlPool>,
    headers: HeaderMap,
    query: Query<DetailListQuery>,
) -> (StatusCode, Json<ApiResponse<Vec<OpenDetailListItem>>>) {
    match v1_get_detail_list(State(pool.clone()), headers, query).await {
        Ok(json_resp) => {
            let inner = json_resp.0;
            if inner.status == 0 {
                success(inner.data.unwrap_or_default())
            } else {
                internal_error("Failed to get detail list")
            }
        }
        Err(e) => handle_v1_err(e),
    }
}

// --- RESTful path-param wrappers ---

pub async fn get_record_by_bangumi(
    State(pool): State<MySqlPool>,
    Path(id): Path<u32>,
    headers: HeaderMap,
    Query(token_q): Query<OpenTokenQuery>,
) -> (StatusCode, Json<ApiResponse<GetRecordData>>) {
    match v1_get_recorder(
        State(pool.clone()),
        headers,
        Query(GetRecorderQuery {
            bangumi_id: Some(id),
            imdb_id: None,
            source: None,
            external_id: None,
            local_bangumi_id: None,
            local_external_media_id: None,
            other_id: None,
            token: token_q.token,
        }),
    )
    .await
    {
        Ok(json_resp) => {
            let inner = json_resp.0;
            if inner.status == 0 {
                success(GetRecordData {
                    source: inner.source,
                    external_id: inner.external_id,
                    local_external_media_id: inner.local_external_media_id,
                    local_bangumi_id: inner.local_bangumi_id,
                    other_id: inner.other_id,
                    bangumi_id: inner.bangumi_id,
                    imdb_id: inner.imdb_id,
                    recorder: inner.recorder,
                    user_status: inner.user_status,
                    is_delete: inner.is_delete,
                    date: inner.date.map(|d| d.and_hms_opt(0, 0, 0).unwrap()),
                })
            } else {
                not_found("Record not found")
            }
        }
        Err(e) => handle_v1_err(e),
    }
}

#[derive(Deserialize)]
pub struct OpenUpdateBody {
    pub recorder: Option<String>,
    pub user_status: Option<i32>,
}

pub async fn update_record_by_bangumi(
    State(pool): State<MySqlPool>,
    Path(id): Path<u32>,
    headers: HeaderMap,
    Query(token_q): Query<OpenTokenQuery>,
    Json(body): Json<OpenUpdateBody>,
) -> (StatusCode, Json<ApiResponse<()>>) {
    match v1_update_recorder(
        State(pool.clone()),
        headers,
        Query(UpdateRecorderQuery {
            bangumi_id: Some(id as i32),
            source: None,
            external_id: None,
            imdb_id: None,
            recorder: body.recorder,
            user_status: body.user_status,
            other_id: None,
            other_title: None,
            other_description: None,
            other_cover: None,
            other_max_number: None,
            other_status: None,
            token: token_q.token,
        }),
    )
    .await
    {
        Ok(json_resp) => {
            let inner = json_resp.0;
            if inner.status == 0 {
                success_empty()
            } else {
                let msg = inner.message.as_deref().unwrap_or("Update failed");
                if inner.status == -1 {
                    bad_request(msg)
                } else if inner.status == -3 {
                    not_found(msg)
                } else {
                    internal_error(msg)
                }
            }
        }
        Err(e) => handle_v1_err(e),
    }
}

pub async fn delete_record_by_bangumi(
    State(pool): State<MySqlPool>,
    Path(id): Path<u32>,
    headers: HeaderMap,
    Query(token_q): Query<OpenTokenQuery>,
) -> (StatusCode, Json<ApiResponse<()>>) {
    match v1_delete_recorder(
        State(pool.clone()),
        headers,
        Query(DeleteRecorderQuery {
            bangumi_id: Some(id),
            source: None,
            external_id: None,
            imdb_id: None,
            other_id: None,
            hard_delete: token_q.hard_delete,
            token: token_q.token,
        }),
    )
    .await
    {
        Ok(json_resp) => {
            let inner = json_resp.0;
            if inner.status == 0 {
                success_empty()
            } else {
                let msg = inner.message.as_deref().unwrap_or("Delete failed");
                if inner.status == -1 {
                    bad_request(msg)
                } else if inner.status == -3 {
                    not_found(msg)
                } else {
                    internal_error(msg)
                }
            }
        }
        Err(e) => handle_v1_err(e),
    }
}

pub async fn get_record_by_imdb(
    State(pool): State<MySqlPool>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Query(token_q): Query<OpenTokenQuery>,
) -> (StatusCode, Json<ApiResponse<GetRecordData>>) {
    match v1_get_recorder(
        State(pool.clone()),
        headers,
        Query(GetRecorderQuery {
            bangumi_id: None,
            imdb_id: Some(id),
            source: None,
            external_id: None,
            local_bangumi_id: None,
            local_external_media_id: None,
            other_id: None,
            token: token_q.token,
        }),
    )
    .await
    {
        Ok(json_resp) => {
            let inner = json_resp.0;
            if inner.status == 0 {
                success(GetRecordData {
                    source: inner.source,
                    external_id: inner.external_id,
                    local_external_media_id: inner.local_external_media_id,
                    local_bangumi_id: inner.local_bangumi_id,
                    other_id: inner.other_id,
                    bangumi_id: inner.bangumi_id,
                    imdb_id: inner.imdb_id,
                    recorder: inner.recorder,
                    user_status: inner.user_status,
                    is_delete: inner.is_delete,
                    date: inner.date.map(|d| d.and_hms_opt(0, 0, 0).unwrap()),
                })
            } else {
                not_found("Record not found")
            }
        }
        Err(e) => handle_v1_err(e),
    }
}

pub async fn update_record_by_imdb(
    State(pool): State<MySqlPool>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Query(token_q): Query<OpenTokenQuery>,
    Json(body): Json<OpenUpdateBody>,
) -> (StatusCode, Json<ApiResponse<()>>) {
    match v1_update_recorder(
        State(pool.clone()),
        headers,
        Query(UpdateRecorderQuery {
            bangumi_id: None,
            source: None,
            external_id: None,
            imdb_id: Some(id),
            recorder: body.recorder,
            user_status: body.user_status,
            other_id: None,
            other_title: None,
            other_description: None,
            other_cover: None,
            other_max_number: None,
            other_status: None,
            token: token_q.token,
        }),
    )
    .await
    {
        Ok(json_resp) => {
            let inner = json_resp.0;
            if inner.status == 0 {
                success_empty()
            } else {
                let msg = inner.message.as_deref().unwrap_or("Update failed");
                if inner.status == -1 {
                    bad_request(msg)
                } else if inner.status == -3 {
                    not_found(msg)
                } else {
                    internal_error(msg)
                }
            }
        }
        Err(e) => handle_v1_err(e),
    }
}

pub async fn delete_record_by_imdb(
    State(pool): State<MySqlPool>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Query(token_q): Query<OpenTokenQuery>,
) -> (StatusCode, Json<ApiResponse<()>>) {
    match v1_delete_recorder(
        State(pool.clone()),
        headers,
        Query(DeleteRecorderQuery {
            bangumi_id: None,
            source: None,
            external_id: None,
            imdb_id: Some(id),
            other_id: None,
            hard_delete: token_q.hard_delete,
            token: token_q.token,
        }),
    )
    .await
    {
        Ok(json_resp) => {
            let inner = json_resp.0;
            if inner.status == 0 {
                success_empty()
            } else {
                let msg = inner.message.as_deref().unwrap_or("Delete failed");
                if inner.status == -1 {
                    bad_request(msg)
                } else if inner.status == -3 {
                    not_found(msg)
                } else {
                    internal_error(msg)
                }
            }
        }
        Err(e) => handle_v1_err(e),
    }
}

pub async fn get_record_by_custom(
    State(pool): State<MySqlPool>,
    Path(id): Path<u32>,
    headers: HeaderMap,
    Query(token_q): Query<OpenTokenQuery>,
) -> (StatusCode, Json<ApiResponse<GetRecordData>>) {
    match v1_get_recorder(
        State(pool.clone()),
        headers,
        Query(GetRecorderQuery {
            bangumi_id: None,
            imdb_id: None,
            source: None,
            external_id: None,
            local_bangumi_id: None,
            local_external_media_id: None,
            other_id: Some(id),
            token: token_q.token,
        }),
    )
    .await
    {
        Ok(json_resp) => {
            let inner = json_resp.0;
            if inner.status == 0 {
                success(GetRecordData {
                    source: inner.source,
                    external_id: inner.external_id,
                    local_external_media_id: inner.local_external_media_id,
                    local_bangumi_id: inner.local_bangumi_id,
                    other_id: inner.other_id,
                    bangumi_id: inner.bangumi_id,
                    imdb_id: inner.imdb_id,
                    recorder: inner.recorder,
                    user_status: inner.user_status,
                    is_delete: inner.is_delete,
                    date: inner.date.map(|d| d.and_hms_opt(0, 0, 0).unwrap()),
                })
            } else {
                not_found("Record not found")
            }
        }
        Err(e) => handle_v1_err(e),
    }
}

pub async fn update_record_by_custom(
    State(pool): State<MySqlPool>,
    Path(id): Path<u32>,
    headers: HeaderMap,
    Query(token_q): Query<OpenTokenQuery>,
    Json(body): Json<OpenUpdateBody>,
) -> (StatusCode, Json<ApiResponse<()>>) {
    match v1_update_recorder(
        State(pool.clone()),
        headers,
        Query(UpdateRecorderQuery {
            bangumi_id: None,
            source: None,
            external_id: None,
            imdb_id: None,
            recorder: body.recorder,
            user_status: body.user_status,
            other_id: Some(id),
            other_title: None,
            other_description: None,
            other_cover: None,
            other_max_number: None,
            other_status: None,
            token: token_q.token,
        }),
    )
    .await
    {
        Ok(json_resp) => {
            let inner = json_resp.0;
            if inner.status == 0 {
                success_empty()
            } else {
                let msg = inner.message.as_deref().unwrap_or("Update failed");
                if inner.status == -1 {
                    bad_request(msg)
                } else if inner.status == -3 {
                    not_found(msg)
                } else {
                    internal_error(msg)
                }
            }
        }
        Err(e) => handle_v1_err(e),
    }
}

pub async fn delete_record_by_custom(
    State(pool): State<MySqlPool>,
    Path(id): Path<u32>,
    headers: HeaderMap,
    Query(token_q): Query<OpenTokenQuery>,
) -> (StatusCode, Json<ApiResponse<()>>) {
    match v1_delete_recorder(
        State(pool.clone()),
        headers,
        Query(DeleteRecorderQuery {
            bangumi_id: None,
            source: None,
            external_id: None,
            imdb_id: None,
            other_id: Some(id),
            hard_delete: token_q.hard_delete,
            token: token_q.token,
        }),
    )
    .await
    {
        Ok(json_resp) => {
            let inner = json_resp.0;
            if inner.status == 0 {
                success_empty()
            } else {
                let msg = inner.message.as_deref().unwrap_or("Delete failed");
                if inner.status == -1 {
                    bad_request(msg)
                } else if inner.status == -3 {
                    not_found(msg)
                } else {
                    internal_error(msg)
                }
            }
        }
        Err(e) => handle_v1_err(e),
    }
}
