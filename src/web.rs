use std::{collections::BTreeSet, net::SocketAddr, sync::Arc};

use askama::Template;
use axum::{
    Form, Router,
    extract::{Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use thiserror::Error;
use tokio::{net::TcpListener, sync::Mutex};
use tracing::{error, info};

use crate::{
    collector::{FeedCollector, RefreshReport},
    config::{
        AppPaths, ConfigError, ConfigStore, FeedConfig, config_path_display, tags_from_input,
    },
    storage::{Storage, StoredArticle},
};

#[derive(Clone)]
struct AppState {
    config: Arc<ConfigStore>,
    storage: Storage,
    collector: FeedCollector,
    refresh_lock: Arc<Mutex<()>>,
    config_path: String,
}

#[derive(Debug, Deserialize, Default)]
struct ArticleQuery {
    tag: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct RefreshForm {
    #[serde(default)]
    tag: String,
}

#[derive(Debug, Deserialize)]
struct FeedForm {
    url: String,
    name: String,
    #[serde(default)]
    tags: String,
}

#[derive(Debug, Deserialize)]
struct UpdateFeedForm {
    original_url: String,
    url: String,
    name: String,
    #[serde(default)]
    tags: String,
}

#[derive(Debug, Deserialize)]
struct DeleteFeedForm {
    url: String,
}

#[derive(Clone)]
struct ArticleView {
    title: String,
    url: String,
    source: String,
    published: String,
    thumbnail_url: String,
    has_thumbnail: bool,
}

#[derive(Clone)]
struct TagView {
    name: String,
    encoded: String,
    active: bool,
}

#[derive(Clone)]
struct FeedView {
    url: String,
    name: String,
    tags: String,
}

#[derive(Default)]
struct Notice {
    text: String,
    is_error: bool,
}

struct ArticlesPanel {
    articles: Vec<ArticleView>,
    tags: Vec<TagView>,
    selected_tag: String,
    all_active: bool,
    notice: Notice,
}

struct FeedList {
    feeds: Vec<FeedView>,
    notice: Notice,
}

#[derive(Template)]
#[template(path = "index.html")]
struct IndexTemplate {
    articles: Vec<ArticleView>,
    tags: Vec<TagView>,
    selected_tag: String,
    all_active: bool,
    notice: Notice,
}

#[derive(Template)]
#[template(path = "articles_panel.html")]
struct ArticlesPanelTemplate {
    articles: Vec<ArticleView>,
    tags: Vec<TagView>,
    selected_tag: String,
    all_active: bool,
    notice: Notice,
}

#[derive(Template)]
#[template(path = "feeds.html")]
struct FeedsTemplate {
    feeds: Vec<FeedView>,
    notice: Notice,
    config_path: String,
}

#[derive(Template)]
#[template(path = "feed_list.html")]
struct FeedListTemplate {
    feeds: Vec<FeedView>,
    notice: Notice,
}

pub async fn serve(address: SocketAddr, paths: AppPaths) -> Result<(), Box<dyn std::error::Error>> {
    let config = Arc::new(ConfigStore::load(&paths.config_file)?);
    let storage = Storage::open(&paths.database_file).await?;
    let collector = FeedCollector::new()?;
    let state = AppState {
        config,
        storage,
        collector,
        refresh_lock: Arc::new(Mutex::new(())),
        config_path: config_path_display(&paths.config_file),
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/articles", get(articles_fragment))
        .route("/refresh", post(refresh))
        .route("/feeds", get(feeds_page).post(add_feed))
        .route("/feeds/update", post(update_feed))
        .route("/feeds/delete", post(delete_feed))
        .route("/static/style.css", get(stylesheet))
        .route("/static/htmx.min.js", get(htmx))
        .with_state(state);

    let listener = TcpListener::bind(address).await?;
    info!(%address, "RSS reader listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn index(
    State(state): State<AppState>,
    Query(query): Query<ArticleQuery>,
) -> Result<Response, AppError> {
    let panel = build_articles_panel(&state, query.tag, Notice::default()).await?;
    render_template(IndexTemplate {
        articles: panel.articles,
        tags: panel.tags,
        selected_tag: panel.selected_tag,
        all_active: panel.all_active,
        notice: panel.notice,
    })
}

async fn articles_fragment(
    State(state): State<AppState>,
    Query(query): Query<ArticleQuery>,
) -> Result<Response, AppError> {
    let panel = build_articles_panel(&state, query.tag, Notice::default()).await?;
    render_articles_panel(panel)
}

async fn refresh(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<RefreshForm>,
) -> Result<Response, AppError> {
    let _guard = state.refresh_lock.lock().await;
    let feeds = state.config.feeds()?;
    let report = state.collector.refresh(feeds, &state.storage).await;
    let notice = refresh_notice(&report);
    let selected_tag = (!form.tag.is_empty()).then_some(form.tag);
    let panel = build_articles_panel(&state, selected_tag, notice).await?;

    if is_htmx(&headers) {
        render_articles_panel(panel)
    } else {
        render_template(IndexTemplate {
            articles: panel.articles,
            tags: panel.tags,
            selected_tag: panel.selected_tag,
            all_active: panel.all_active,
            notice: panel.notice,
        })
    }
}

async fn feeds_page(State(state): State<AppState>) -> Result<Response, AppError> {
    let list = build_feed_list(&state, Notice::default())?;
    render_template(FeedsTemplate {
        feeds: list.feeds,
        notice: list.notice,
        config_path: state.config_path,
    })
}

async fn add_feed(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<FeedForm>,
) -> Result<Response, AppError> {
    let result = state.config.add(FeedConfig {
        url: form.url,
        name: form.name,
        tags: tags_from_input(&form.tags),
    });
    let notice = operation_notice(result, "Feed added. Use refresh to retrieve its articles.");
    render_feed_response(&state, &headers, notice)
}

async fn update_feed(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<UpdateFeedForm>,
) -> Result<Response, AppError> {
    let _guard = state.refresh_lock.lock().await;
    let original_url = form.original_url;
    let new_name = form.name.trim().to_owned();
    let normalized_new_url = url::Url::parse(form.url.trim())
        .map(|url| url.to_string())
        .unwrap_or_else(|_| form.url.clone());
    let result = state.config.update(
        &original_url,
        FeedConfig {
            url: form.url,
            name: form.name,
            tags: tags_from_input(&form.tags),
        },
    );
    let notice = match result {
        Err(error) => Notice {
            text: error.to_string(),
            is_error: true,
        },
        Ok(()) => {
            let cache_result = if original_url == normalized_new_url {
                state
                    .storage
                    .update_source_name(&original_url, &new_name)
                    .await
            } else {
                state.storage.delete_feed(&original_url).await
            };
            cache_operation_notice(
                cache_result,
                "Feed updated.",
                "Feed updated, but cached article metadata could not be changed. Retry saving or refresh the feed.",
            )
        }
    };
    render_feed_response(&state, &headers, notice)
}

async fn delete_feed(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<DeleteFeedForm>,
) -> Result<Response, AppError> {
    let _guard = state.refresh_lock.lock().await;
    let result = state.config.delete(&form.url);
    let notice = match result {
        Err(error) => Notice {
            text: error.to_string(),
            is_error: true,
        },
        Ok(()) => cache_operation_notice(
            state.storage.delete_feed(&form.url).await,
            "Feed deleted.",
            "Feed deleted, but its hidden cache rows could not be removed.",
        ),
    };
    render_feed_response(&state, &headers, notice)
}

fn render_feed_response(
    state: &AppState,
    headers: &HeaderMap,
    notice: Notice,
) -> Result<Response, AppError> {
    let list = build_feed_list(state, notice)?;
    if is_htmx(headers) {
        render_template(FeedListTemplate {
            feeds: list.feeds,
            notice: list.notice,
        })
    } else {
        render_template(FeedsTemplate {
            feeds: list.feeds,
            notice: list.notice,
            config_path: state.config_path.clone(),
        })
    }
}

async fn build_articles_panel(
    state: &AppState,
    selected_tag: Option<String>,
    notice: Notice,
) -> Result<ArticlesPanel, AppError> {
    let feeds = state.config.feeds()?;
    let selected_tag = selected_tag.unwrap_or_default();
    let feed_urls = feeds
        .iter()
        .filter(|feed| selected_tag.is_empty() || feed.tags.contains(&selected_tag))
        .map(|feed| feed.url.clone())
        .collect::<Vec<_>>();
    let articles = state
        .storage
        .list_articles(&feed_urls)
        .await?
        .into_iter()
        .map(article_view)
        .collect();
    let all_active = selected_tag.is_empty();
    let tags = feeds
        .iter()
        .flat_map(|feed| feed.tags.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|name| TagView {
            encoded: url::form_urlencoded::byte_serialize(name.as_bytes()).collect(),
            active: name == selected_tag,
            name,
        })
        .collect();

    Ok(ArticlesPanel {
        articles,
        tags,
        selected_tag,
        all_active,
        notice,
    })
}

fn article_view(article: StoredArticle) -> ArticleView {
    let published = DateTime::<Utc>::from_timestamp(article.published_at, 0)
        .unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
        .format("%b %-d, %Y, %H:%M UTC")
        .to_string();
    let thumbnail_url = article.thumbnail_url.unwrap_or_default();
    ArticleView {
        title: article.title,
        url: article.url,
        source: article.source_name,
        published,
        has_thumbnail: !thumbnail_url.is_empty(),
        thumbnail_url,
    }
}

fn build_feed_list(state: &AppState, notice: Notice) -> Result<FeedList, AppError> {
    let mut feeds = state
        .config
        .feeds()?
        .into_iter()
        .map(|feed| FeedView {
            url: feed.url,
            name: feed.name,
            tags: feed.tags.join(", "),
        })
        .collect::<Vec<_>>();
    feeds.sort_by_key(|feed| feed.name.to_lowercase());
    Ok(FeedList { feeds, notice })
}

fn refresh_notice(report: &RefreshReport) -> Notice {
    if report.successful_feeds == 0 && report.failures.is_empty() {
        return Notice {
            text: "No feeds are configured yet.".to_owned(),
            is_error: false,
        };
    }

    let mut text = format!(
        "Refreshed {} feed(s) and processed {} article(s).",
        report.successful_feeds, report.articles_seen
    );
    if !report.failures.is_empty() {
        let failures = report
            .failures
            .iter()
            .map(|failure| format!("{}: {}", failure.name, failure.reason))
            .collect::<Vec<_>>()
            .join("; ");
        text.push_str(&format!(" Failed: {failures}"));
    }
    Notice {
        text,
        is_error: !report.failures.is_empty(),
    }
}

fn operation_notice(result: Result<(), ConfigError>, success: &str) -> Notice {
    match result {
        Ok(()) => Notice {
            text: success.to_owned(),
            is_error: false,
        },
        Err(error) => Notice {
            text: error.to_string(),
            is_error: true,
        },
    }
}

fn cache_operation_notice(result: Result<(), sqlx::Error>, success: &str, failure: &str) -> Notice {
    match result {
        Ok(()) => Notice {
            text: success.to_owned(),
            is_error: false,
        },
        Err(error) => {
            error!(%error, "feed configuration changed but cache maintenance failed");
            Notice {
                text: failure.to_owned(),
                is_error: true,
            }
        }
    }
}

fn render_articles_panel(panel: ArticlesPanel) -> Result<Response, AppError> {
    render_template(ArticlesPanelTemplate {
        articles: panel.articles,
        tags: panel.tags,
        selected_tag: panel.selected_tag,
        all_active: panel.all_active,
        notice: panel.notice,
    })
}

fn render_template(template: impl Template) -> Result<Response, AppError> {
    Ok(Html(template.render()?).into_response())
}

fn is_htmx(headers: &HeaderMap) -> bool {
    headers
        .get("HX-Request")
        .is_some_and(|value| value == "true")
}

async fn stylesheet() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("../static/style.css"),
    )
}

async fn htmx() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("../static/htmx.min.js"),
    )
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        error!(%error, "could not install shutdown signal handler");
    }
}

#[derive(Debug, Error)]
enum AppError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Template(#[from] askama::Error),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        error!(error = %self, "request failed");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "The request could not be completed.",
        )
            .into_response()
    }
}
