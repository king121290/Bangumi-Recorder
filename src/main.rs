use axum::{
    Router,
    http::{StatusCode, header},
    middleware,
    response::IntoResponse,
    routing::{get, patch, post, put},
};
use chrono::{Days, Local, NaiveTime};
use clap::Parser;
use dotenvy::dotenv;
use http::Method;
use rust_embed::RustEmbed;
use serde::Serialize;
use sqlx::MySqlPool;
use std::time::Duration;
use tower_http::cors::CorsLayer;

mod api;
mod auth_bearer;

use std::sync::{Arc, OnceLock};

static EXTERNAL_FRONTEND: OnceLock<String> = OnceLock::new();

#[derive(Parser)]
#[command(name = "Bangumi-Recorder", version = VERSION)]
struct Cli {
    /// Frontend source: a URL (http://...) to redirect to, or a local
    /// directory path to serve static files from.
    /// When omitted, the embedded frontend build is served.
    #[arg(long, short = 'f', value_name = "PATH_OR_URL")]
    frontend: Option<String>,
}

pub const VERSION: &str = concat!(
    env!("BUILD_RUSTC_VERSION"),
    " (",
    env!("BUILD_GIT_VERSION"),
    ")"
);

#[derive(RustEmbed)]
#[folder = "frontend/dist"]
struct Assets;

fn guess_mime(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js" | "mjs") => "application/javascript; charset=utf-8",
        Some("json") => "application/json",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        Some("wasm") => "application/wasm",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        _ => "application/octet-stream",
    }
}

fn seconds_until_next_midnight() -> u64 {
    let now = Local::now().naive_local();
    let next_midnight = now
        .date()
        .checked_add_days(Days::new(1))
        .expect("next day should be representable")
        .and_time(NaiveTime::MIN);
    (next_midnight - now).num_seconds().max(1) as u64
}

#[tokio::main]
async fn main() {
    dotenv().ok();

    let cli = Cli::parse();
    if let Some(val) = cli.frontend {
        let _ = EXTERNAL_FRONTEND.set(val);
    }

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");

    let pool = MySqlPool::connect(&database_url)
        .await
        .expect("Failed to connect to database");

    if let Err(e) = api::logs::cleanup_soft_deleted_records(&pool, None).await {
        log::warn!("Failed to run startup cleanup: {}", e);
    }

    let cleanup_pool = pool.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(seconds_until_next_midnight())).await;
            if let Err(e) = api::logs::cleanup_soft_deleted_records(&cleanup_pool, None).await {
                log::warn!("Failed to run scheduled cleanup: {}", e);
            }
        }
    });

    let listen = format!(
        "{}:{}",
        std::env::var("LISTEN").unwrap_or_else(|_| "127.0.0.1".to_string()),
        std::env::var("LISTEN_PORT").unwrap_or_else(|_| "8080".to_string())
    );

    let jwt_secret: Arc<str> = std::env::var("JWT_SECRET")
        .expect("JWT_SECRET must be set")
        .into();
    let jwt_secret_v2 = Arc::clone(&jwt_secret);

    async fn index() -> impl IntoResponse {
        if let Some(frontend) = EXTERNAL_FRONTEND.get() {
            if frontend.starts_with("http://") || frontend.starts_with("https://") {
                return axum::response::Redirect::temporary(frontend.as_str()).into_response();
            }
            // Serve index.html from local path
            let index = std::path::Path::new(frontend).join("index.html");
            if let Ok(content) = tokio::fs::read(&index).await {
                return (
                    [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                    content,
                )
                    .into_response();
            }
            return (StatusCode::NOT_FOUND, "Frontend assets not found").into_response();
        }
        let content = Assets::get("index.html")
            .expect("index.html not found in embedded assets")
            .data;
        (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            content.into_owned(),
        )
            .into_response()
    }

    #[derive(Serialize)]
    struct VersionInfo {
        version: &'static str,
        rustc: &'static str,
        git: &'static str,
    }

    async fn version_handler() -> impl IntoResponse {
        let info = VersionInfo {
            version: env!("CARGO_PKG_VERSION"),
            rustc: env!("BUILD_RUSTC_VERSION"),
            git: env!("BUILD_GIT_VERSION"),
        };
        (StatusCode::OK, axum::Json(info))
    }

    async fn serve_fallback(uri: axum::http::Uri) -> impl IntoResponse {
        if let Some(frontend) = EXTERNAL_FRONTEND.get() {
            if frontend.starts_with("http://") || frontend.starts_with("https://") {
                let dest = format!("{}{}", frontend.trim_end_matches('/'), uri.path());
                return axum::response::Redirect::temporary(&dest).into_response();
            }
            // Serve static files from a local directory (SPA style)
            let path = uri.path().trim_start_matches('/');
            let requested_path = if path.is_empty() { "index.html" } else { path };
            if !std::path::Path::new(requested_path)
                .components()
                .all(|component| matches!(component, std::path::Component::Normal(_)))
            {
                return (StatusCode::BAD_REQUEST, "Invalid asset path").into_response();
            }
            let file_path = std::path::Path::new(frontend).join(requested_path);
            if file_path.is_file()
                && let Ok(content) = tokio::fs::read(&file_path).await
            {
                let name = file_path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                let mime = guess_mime(name);
                return ([(header::CONTENT_TYPE, mime)], content).into_response();
            }
            // SPA fallback: serve index.html
            let index = std::path::Path::new(frontend).join("index.html");
            if let Ok(content) = tokio::fs::read(&index).await {
                return (
                    [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                    content,
                )
                    .into_response();
            }
            return (StatusCode::NOT_FOUND, "Frontend assets not found").into_response();
        }
        // Embedded assets
        let path = uri.path().trim_start_matches('/');
        if !path.is_empty()
            && let Some(content) = Assets::get(path)
        {
            let mime = guess_mime(path);
            return ([(header::CONTENT_TYPE, mime)], content.data.into_owned()).into_response();
        }
        let content = Assets::get("index.html")
            .expect("index.html not found in embedded assets")
            .data;
        (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            content.into_owned(),
        )
            .into_response()
    }

    let api_router = Router::new()
        .route(
            "/search/bangumi",
            post(api::search::search_bangumi_by_title),
        )
        .route(
            "/search/bangumi/id",
            post(api::search::search_bangumi_by_id),
        )
        .route("/search/imdb", post(api::imdb::search_imdb))
        .route("/search/imdb/id", post(api::imdb::search_imdb_by_id))
        .route("/search/local", post(api::search::search_local))
        .route("/record/add", post(api::new::add_record))
        .route(
            "/record/update",
            post(api::update_recorder::update_user_recorder),
        )
        .route(
            "/record/delete",
            post(api::delete_recorder::delete_recorder),
        )
        .route("/record/get", post(api::get_recorder::get_recorder))
        .route("/record/list", get(api::list::list_recorder))
        .route(
            "/record/detail_list",
            get(api::detail_list::get_detail_list),
        )
        .route(
            "/auth/token/regenerate",
            post(auth_bearer::regenerate_api_token),
        )
        .route("/user/info", get(api::user::get_info))
        .route("/user/update", post(api::user::update_info))
        .route("/user/password", post(api::user::update_password))
        .with_state(pool.clone())
        .layer(middleware::from_fn(move |req, next| {
            let jwt_secret = Arc::clone(&jwt_secret);
            async move { auth_bearer::jwt_auth(req, next, jwt_secret).await }
        }));

    let open_router = Router::new()
        .route("/new", post(api::open::new::add_record_open))
        .route(
            "/update",
            post(api::open::update_recorder::update_user_recorder),
        )
        .route("/delete", post(api::open::delete_recorder::delete_recorder))
        .route("/get", post(api::open::get_recorder::get_recorder))
        .route("/list", get(api::open::list::list_recorder))
        .route("/detail_list", get(api::open::detail_list::get_detail_list))
        .route("/user/info", get(api::open::user::get_info))
        .with_state(pool.clone());

    let v2_api_router = Router::new()
        // Search & Bangumi
        .route("/search", get(api::v2::search::search_bangumi))
        .route("/search/local", get(api::v2::search::search_local))
        .route("/bangumi/:id", get(api::v2::search::get_bangumi))
        .route(
            "/bangumi/:id/refresh",
            post(api::v2::search::refresh_bangumi),
        )
        .route(
            "/bangumi/:id/episodes",
            get(api::v2::search::get_bangumi_episodes),
        )
        .route("/imdb/search", get(api::v2::search::search_imdb))
        .route("/imdb/:id", get(api::v2::search::get_imdb))
        .route("/other/:id", get(api::v2::search::get_other))
        // Records
        .route(
            "/records",
            get(api::v2::record::list_recorder).post(api::v2::record::add_record),
        )
        .route("/records/detail", get(api::v2::record::get_detail_list))
        .route(
            "/records/bangumi/:id",
            get(api::v2::record::get_record_by_bangumi)
                .patch(api::v2::record::update_record_by_bangumi)
                .delete(api::v2::record::delete_record_by_bangumi),
        )
        .route(
            "/records/imdb/:id",
            get(api::v2::record::get_record_by_imdb)
                .patch(api::v2::record::update_record_by_imdb)
                .delete(api::v2::record::delete_record_by_imdb),
        )
        .route(
            "/records/custom/:id",
            get(api::v2::record::get_record_by_custom)
                .patch(api::v2::record::update_record_by_custom)
                .delete(api::v2::record::delete_record_by_custom),
        )
        // Per-user episode tracking
        .route(
            "/records/bangumi/:id/episodes",
            get(api::v2::episodes::list_episodes),
        )
        .route(
            "/records/bangumi/:id/episodes/:ordinal",
            patch(api::v2::episodes::update_episode),
        )
        // Sync
        .route("/sync", post(api::v2::sync::sync_records))
        .route("/sync_ani", post(api::v2::sync_ani::sync_records))
        .route("/sync/incremental", get(api::v2::sync::incremental_sync))
        // User profile
        .route(
            "/me",
            get(api::v2::user::get_info).patch(api::v2::user::update_info),
        )
        .route("/me/password", put(api::v2::user::update_password))
        .route("/me/token", post(api::v2::user::regenerate_api_token))
        .route("/logs/recordings", get(api::logs::list_recording_logs))
        .route("/logs/system", get(api::logs::list_system_logs))
        .route(
            "/settings/auto-cleanup",
            get(api::logs::get_auto_cleanup_setting).put(api::logs::update_auto_cleanup_setting),
        )
        // API Token management (multi-token)
        .route(
            "/tokens",
            get(api::v2::token::list_tokens).post(api::v2::token::create_token),
        )
        .route(
            "/tokens/:id",
            put(api::v2::token::update_token).delete(api::v2::token::delete_token),
        )
        .route(
            "/tokens/permissions",
            get(api::v2::token::permission_labels),
        )
        .with_state(pool.clone())
        .layer(middleware::from_fn(move |req, next| {
            let jwt_secret = Arc::clone(&jwt_secret_v2);
            async move { auth_bearer::jwt_auth(req, next, jwt_secret).await }
        }));

    let v2_open_router = Router::new()
        .route(
            "/records",
            post(api::v2::open::record::add_record).get(api::v2::open::record::list_recorder),
        )
        .route(
            "/records/detail",
            get(api::v2::open::record::get_detail_list),
        )
        .route(
            "/records/bangumi/:id",
            get(api::v2::open::record::get_record_by_bangumi)
                .patch(api::v2::open::record::update_record_by_bangumi)
                .delete(api::v2::open::record::delete_record_by_bangumi),
        )
        .route(
            "/records/imdb/:id",
            get(api::v2::open::record::get_record_by_imdb)
                .patch(api::v2::open::record::update_record_by_imdb)
                .delete(api::v2::open::record::delete_record_by_imdb),
        )
        .route(
            "/records/custom/:id",
            get(api::v2::open::record::get_record_by_custom)
                .patch(api::v2::open::record::update_record_by_custom)
                .delete(api::v2::open::record::delete_record_by_custom),
        )
        .route("/me", get(api::v2::open::user::get_info))
        .route("/search", get(api::v2::open::search::search_bangumi))
        .route("/bangumi/:id", get(api::v2::open::search::get_bangumi))
        .route(
            "/bangumi/:id/episodes",
            get(api::v2::open::search::get_bangumi_episodes),
        )
        .route("/imdb/search", get(api::v2::open::search::search_imdb))
        .route("/imdb/:id", get(api::v2::open::search::get_imdb))
        .route("/other/:id", get(api::v2::open::search::get_other))
        .route("/search/local", get(api::v2::open::search::search_local))
        .route(
            "/logs/recordings",
            get(api::v2::open::logs::list_recording_logs_open),
        )
        .route(
            "/logs/system",
            get(api::v2::open::logs::list_system_logs_open),
        )
        // Episodes (per-user tracking)
        .route(
            "/episodes/:bangumi_id",
            get(api::v2::open::episodes::list_episodes),
        )
        .route(
            "/episodes/:bangumi_id/:ordinal",
            patch(api::v2::open::episodes::update_episode),
        )
        // Sync
        .route(
            "/sync",
            post(api::v2::open::sync::sync_records).get(api::v2::open::sync::incremental_sync),
        )
        .route("/sync_ani", post(api::v2::open::sync::sync_ani_records))
        .with_state(pool.clone());

    let v2_auth_router = Router::new()
        .route("/login", post(api::v2::auth::login))
        .route("/register", post(api::v2::auth::register))
        .route("/config", get(api::v2::auth::get_config));

    let app = Router::new()
        .route("/", get(index))
        .route("/auth/login", post(auth_bearer::login))
        .route("/auth/register", post(auth_bearer::register))
        .route("/auth/config", get(auth_bearer::get_config))
        .route("/api/v2/version", get(version_handler))
        .nest("/api/v1", api_router)
        .nest("/api/v1/open", open_router)
        .nest("/api/v2", v2_api_router)
        .nest("/api/v2/open", v2_open_router)
        .nest("/api/v2/auth", v2_auth_router)
        .fallback(get(serve_fallback))
        .with_state(pool)
        .layer(CorsLayer::permissive().allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ]));

    log::info!("{}", VERSION);
    log::info!("Listening on http://{}", listen);

    let listener = tokio::net::TcpListener::bind(&listen).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
