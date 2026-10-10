use std::{collections::BTreeSet, net::SocketAddr, sync::Arc};

use askama::Template;
use axum::{
    Form, Router,
    extract::{Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Deserialize;
use thiserror::Error;
use tokio::{net::TcpListener, sync::Mutex};
use tracing::{error, info};

use crate::{
    collector::{FeedCollector, RefreshReport},
    config::{
        AppPaths, ConfigError, ConfigStore, FeedConfig, config_path_display, tags_from_input,
    },
    storage::{ReadFilter, Storage, StoredArticle},
};

#[derive(Clone)]
struct AppState {
    config: Arc<ConfigStore>,
    storage: Storage,
    collector: FeedCollector,
    refresh_lock: Arc<Mutex<()>>,
    config_path: String,
    csrf_token: String,
}

#[derive(Debug, Deserialize, Default)]
struct ArticleQuery {
    feed: Option<String>,
    tag: Option<String>,
    status: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct FeedQuery {
    tag: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct RefreshForm {
    #[serde(default)]
    feed: String,
    #[serde(default)]
    tag: String,
    #[serde(default)]
    status: String,
    csrf_token: String,
}

#[derive(Debug, Deserialize)]
struct OpenArticleQuery {
    url: String,
    csrf_token: String,
}

#[derive(Debug, Deserialize)]
struct ReadArticleForm {
    url: String,
    action: ReadAction,
    #[serde(default)]
    feed: String,
    #[serde(default)]
    tag: String,
    #[serde(default)]
    status: String,
    csrf_token: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ReadAction {
    Read,
    Unread,
}

#[derive(Debug, Deserialize)]
struct FeedForm {
    url: String,
    name: String,
    #[serde(default)]
    tags: String,
    csrf_token: String,
}

#[derive(Debug, Deserialize)]
struct UpdateFeedForm {
    original_url: String,
    url: String,
    name: String,
    #[serde(default)]
    tags: String,
    csrf_token: String,
}

#[derive(Debug, Deserialize)]
struct DeleteFeedForm {
    url: String,
    csrf_token: String,
}

#[derive(Clone)]
struct ArticleView {
    title: String,
    url: String,
    source: String,
    published: String,
    published_datetime: String,
    published_relative: String,
    thumbnail_url: String,
    has_thumbnail: bool,
    open_url: String,
    is_read: bool,
}

#[derive(Clone)]
struct TagView {
    name: String,
    href: String,
    fragment_href: String,
    active: bool,
}

#[derive(Clone)]
struct ReadFilterView {
    name: &'static str,
    href: String,
    fragment_href: String,
    active: bool,
}

#[derive(Clone, Debug, Default)]
struct ArticleFilters {
    feed: String,
    tag: String,
    read_filter: ReadFilter,
}

#[derive(Clone)]
struct FeedDirectoryView {
    name: String,
    url: String,
    tags: Vec<String>,
    posts_href: String,
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
    selected_feed: String,
    selected_feed_name: String,
    selected_tag: String,
    selected_status: String,
    all_feeds_href: String,
    all_feeds_fragment_href: String,
    all_active: bool,
    all_tags_href: String,
    all_tags_fragment_href: String,
    read_filters: Vec<ReadFilterView>,
    csrf_token: String,
    notice: Notice,
}

struct FeedsPanel {
    feeds: Vec<FeedDirectoryView>,
    tags: Vec<TagView>,
    selected_tag: String,
    all_active: bool,
    all_tags_href: String,
    all_tags_fragment_href: String,
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
    selected_feed: String,
    selected_feed_name: String,
    selected_tag: String,
    selected_status: String,
    all_feeds_href: String,
    all_feeds_fragment_href: String,
    all_active: bool,
    all_tags_href: String,
    all_tags_fragment_href: String,
    read_filters: Vec<ReadFilterView>,
    csrf_token: String,
    notice: Notice,
    posts_nav_active: bool,
    feeds_nav_active: bool,
    sources_nav_active: bool,
}

#[derive(Template)]
#[template(path = "articles_panel.html")]
struct ArticlesPanelTemplate {
    articles: Vec<ArticleView>,
    tags: Vec<TagView>,
    selected_feed: String,
    selected_feed_name: String,
    selected_tag: String,
    selected_status: String,
    all_feeds_href: String,
    all_feeds_fragment_href: String,
    all_active: bool,
    all_tags_href: String,
    all_tags_fragment_href: String,
    read_filters: Vec<ReadFilterView>,
    csrf_token: String,
    notice: Notice,
}

#[derive(Template)]
#[template(path = "feeds.html")]
struct FeedsTemplate {
    feeds: Vec<FeedDirectoryView>,
    tags: Vec<TagView>,
    selected_tag: String,
    all_active: bool,
    all_tags_href: String,
    all_tags_fragment_href: String,
    posts_nav_active: bool,
    feeds_nav_active: bool,
    sources_nav_active: bool,
}

#[derive(Template)]
#[template(path = "feeds_panel.html")]
struct FeedsPanelTemplate {
    feeds: Vec<FeedDirectoryView>,
    tags: Vec<TagView>,
    selected_tag: String,
    all_active: bool,
    all_tags_href: String,
    all_tags_fragment_href: String,
}

#[derive(Template)]
#[template(path = "sources.html")]
struct SourcesTemplate {
    feeds: Vec<FeedView>,
    notice: Notice,
    config_path: String,
    csrf_token: String,
    posts_nav_active: bool,
    feeds_nav_active: bool,
    sources_nav_active: bool,
}

#[derive(Template)]
#[template(path = "feed_list.html")]
struct FeedListTemplate {
    feeds: Vec<FeedView>,
    notice: Notice,
    csrf_token: String,
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
        csrf_token: uuid::Uuid::new_v4().simple().to_string(),
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/articles", get(articles_fragment))
        .route("/articles/open", get(open_article))
        .route("/articles/read", post(set_article_read))
        .route("/refresh", post(refresh))
        .route("/feeds", get(feeds_page))
        .route("/feeds/list", get(feeds_fragment))
        .route("/sources", get(sources_page).post(add_feed))
        .route("/sources/update", post(update_feed))
        .route("/sources/delete", post(delete_feed))
        .route("/static/style.css", get(stylesheet))
        .route("/static/htmx.min.js", get(htmx))
        .route("/static/navigation.js", get(navigation_script))
        .with_state(state);

    let listener = TcpListener::bind(address).await?;
    info!(%address, "Blatt listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn index(
    State(state): State<AppState>,
    Query(query): Query<ArticleQuery>,
) -> Result<Response, AppError> {
    let read_filter = read_filter_from_status(query.status.as_deref());
    let filters = ArticleFilters {
        feed: query.feed.unwrap_or_default(),
        tag: query.tag.unwrap_or_default(),
        read_filter,
    };
    let panel = build_articles_panel(&state, &filters, Notice::default()).await?;
    render_index(panel)
}

async fn articles_fragment(
    State(state): State<AppState>,
    Query(query): Query<ArticleQuery>,
) -> Result<Response, AppError> {
    let read_filter = read_filter_from_status(query.status.as_deref());
    let filters = ArticleFilters {
        feed: query.feed.unwrap_or_default(),
        tag: query.tag.unwrap_or_default(),
        read_filter,
    };
    let panel = build_articles_panel(&state, &filters, Notice::default()).await?;
    render_articles_panel(panel)
}

async fn open_article(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<OpenArticleQuery>,
) -> Result<Response, AppError> {
    verify_csrf(&state, &query.csrf_token)?;
    verify_user_navigation(&headers)?;
    let destination = state
        .storage
        .mark_article_read(&query.url)
        .await?
        .ok_or(AppError::ArticleNotFound)?;
    let mut response = Redirect::temporary(&destination).into_response();
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    Ok(response)
}

async fn set_article_read(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<ReadArticleForm>,
) -> Result<Response, AppError> {
    verify_csrf(&state, &form.csrf_token)?;
    let (read, message) = match form.action {
        ReadAction::Read => (true, "Article marked as read."),
        ReadAction::Unread => (false, "Article marked as unread."),
    };
    if !state.storage.set_article_read(&form.url, read).await? {
        return Err(AppError::ArticleNotFound);
    }

    let filters = ArticleFilters {
        feed: form.feed,
        tag: form.tag,
        read_filter: read_filter_from_status(Some(&form.status)),
    };
    let panel = build_articles_panel(
        &state,
        &filters,
        Notice {
            text: message.to_owned(),
            is_error: false,
        },
    )
    .await?;
    if is_htmx(&headers) {
        render_articles_panel(panel)
    } else {
        Ok(Redirect::to(&articles_url("/", &filters)).into_response())
    }
}

async fn refresh(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<RefreshForm>,
) -> Result<Response, AppError> {
    verify_csrf(&state, &form.csrf_token)?;
    let _guard = state.refresh_lock.lock().await;
    let feeds = state.config.feeds()?;
    let report = state.collector.refresh(feeds, &state.storage).await;
    let notice = refresh_notice(&report);
    let filters = ArticleFilters {
        feed: form.feed,
        tag: form.tag,
        read_filter: read_filter_from_status(Some(&form.status)),
    };
    let panel = build_articles_panel(&state, &filters, notice).await?;
    if is_htmx(&headers) {
        render_articles_panel(panel)
    } else {
        Ok(Redirect::to(&articles_url("/", &filters)).into_response())
    }
}

async fn feeds_page(
    State(state): State<AppState>,
    Query(query): Query<FeedQuery>,
) -> Result<Response, AppError> {
    let panel = build_feeds_panel(&state, query.tag)?;
    render_template(FeedsTemplate {
        feeds: panel.feeds,
        tags: panel.tags,
        selected_tag: panel.selected_tag,
        all_active: panel.all_active,
        all_tags_href: panel.all_tags_href,
        all_tags_fragment_href: panel.all_tags_fragment_href,
        posts_nav_active: false,
        feeds_nav_active: true,
        sources_nav_active: false,
    })
}

async fn feeds_fragment(
    State(state): State<AppState>,
    Query(query): Query<FeedQuery>,
) -> Result<Response, AppError> {
    render_feeds_panel(build_feeds_panel(&state, query.tag)?)
}

async fn sources_page(State(state): State<AppState>) -> Result<Response, AppError> {
    let list = build_feed_list(&state, Notice::default())?;
    render_template(SourcesTemplate {
        feeds: list.feeds,
        notice: list.notice,
        config_path: state.config_path,
        csrf_token: state.csrf_token,
        posts_nav_active: false,
        feeds_nav_active: false,
        sources_nav_active: true,
    })
}

async fn add_feed(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<FeedForm>,
) -> Result<Response, AppError> {
    verify_csrf(&state, &form.csrf_token)?;
    let result = state.config.add(FeedConfig {
        url: form.url,
        name: form.name,
        tags: tags_from_input(&form.tags),
    });
    let notice = operation_notice(result, "Feed added. Use refresh to retrieve its articles.");
    render_source_response(&state, &headers, notice)
}

async fn update_feed(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<UpdateFeedForm>,
) -> Result<Response, AppError> {
    verify_csrf(&state, &form.csrf_token)?;
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
    render_source_response(&state, &headers, notice)
}

async fn delete_feed(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<DeleteFeedForm>,
) -> Result<Response, AppError> {
    verify_csrf(&state, &form.csrf_token)?;
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
    render_source_response(&state, &headers, notice)
}

fn render_source_response(
    state: &AppState,
    headers: &HeaderMap,
    notice: Notice,
) -> Result<Response, AppError> {
    let list = build_feed_list(state, notice)?;
    if is_htmx(headers) {
        render_template(FeedListTemplate {
            feeds: list.feeds,
            notice: list.notice,
            csrf_token: state.csrf_token.clone(),
        })
    } else {
        render_template(SourcesTemplate {
            feeds: list.feeds,
            notice: list.notice,
            config_path: state.config_path.clone(),
            csrf_token: state.csrf_token.clone(),
            posts_nav_active: false,
            feeds_nav_active: false,
            sources_nav_active: true,
        })
    }
}

async fn build_articles_panel(
    state: &AppState,
    filters: &ArticleFilters,
    notice: Notice,
) -> Result<ArticlesPanel, AppError> {
    let feeds = state.config.feeds()?;
    let feed_urls = selected_feed_urls(&feeds, filters);
    let now = Utc::now();
    let articles = state
        .storage
        .list_articles(&feed_urls, filters.read_filter)
        .await?
        .into_iter()
        .map(|article| article_view(article, &state.csrf_token, now))
        .collect();
    let selected_feed_name = if filters.feed.is_empty() {
        String::new()
    } else {
        feeds
            .iter()
            .find(|feed| feed_name_matches(&feed.name, &filters.feed))
            .map(|feed| feed.name.clone())
            .unwrap_or_else(|| "Unavailable feed".to_owned())
    };
    let all_active = filters.tag.is_empty();
    let tags = feeds
        .iter()
        .flat_map(|feed| feed.tags.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|name| {
            let mut tag_filters = filters.clone();
            tag_filters.tag = name.clone();
            TagView {
                href: articles_url("/", &tag_filters),
                fragment_href: articles_url("/articles", &tag_filters),
                active: name == filters.tag,
                name,
            }
        })
        .collect();
    let read_filters = [
        (ReadFilter::All, "All"),
        (ReadFilter::Unread, "Unread"),
        (ReadFilter::Read, "Read"),
    ]
    .into_iter()
    .map(|(filter, name)| {
        let mut status_filters = filters.clone();
        status_filters.read_filter = filter;
        ReadFilterView {
            name,
            href: articles_url("/", &status_filters),
            fragment_href: articles_url("/articles", &status_filters),
            active: filter == filters.read_filter,
        }
    })
    .collect();

    let mut all_tags_filters = filters.clone();
    all_tags_filters.tag.clear();
    let mut all_feeds_filters = filters.clone();
    all_feeds_filters.feed.clear();

    Ok(ArticlesPanel {
        articles,
        tags,
        selected_feed: filters.feed.clone(),
        selected_feed_name,
        selected_status: read_filter_status(filters.read_filter).to_owned(),
        selected_tag: filters.tag.clone(),
        all_feeds_href: articles_url("/", &all_feeds_filters),
        all_feeds_fragment_href: articles_url("/articles", &all_feeds_filters),
        all_active,
        all_tags_href: articles_url("/", &all_tags_filters),
        all_tags_fragment_href: articles_url("/articles", &all_tags_filters),
        read_filters,
        csrf_token: state.csrf_token.clone(),
        notice,
    })
}

fn article_view(article: StoredArticle, csrf_token: &str, now: DateTime<Utc>) -> ArticleView {
    let published_at = DateTime::<Utc>::from_timestamp(article.published_at, 0)
        .unwrap_or(DateTime::<Utc>::UNIX_EPOCH);
    let published = published_at.format("%b %-d, %Y, %H:%M UTC").to_string();
    let published_datetime = published_at.to_rfc3339_opts(SecondsFormat::Secs, true);
    let published_relative = relative_time(published_at, now);
    let thumbnail_url = article.thumbnail_url.unwrap_or_default();
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.append_pair("url", &article.url);
    query.append_pair("csrf_token", csrf_token);
    let open_url = format!("/articles/open?{}", query.finish());
    ArticleView {
        title: article.title,
        url: article.url,
        source: article.source_name,
        published,
        published_datetime,
        published_relative,
        has_thumbnail: !thumbnail_url.is_empty(),
        thumbnail_url,
        open_url,
        is_read: article.is_read,
    }
}

fn relative_time(published_at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let seconds = now.signed_duration_since(published_at).num_seconds();
    if seconds.unsigned_abs() < 60 {
        return "just now".to_owned();
    }

    let elapsed = seconds.unsigned_abs();
    let (value, unit) = if elapsed < 60 * 60 {
        (elapsed / 60, "minute")
    } else if elapsed < 24 * 60 * 60 {
        (elapsed / (60 * 60), "hour")
    } else if elapsed < 7 * 24 * 60 * 60 {
        (elapsed / (24 * 60 * 60), "day")
    } else if elapsed < 30 * 24 * 60 * 60 {
        (elapsed / (7 * 24 * 60 * 60), "week")
    } else if elapsed < 365 * 24 * 60 * 60 {
        (elapsed / (30 * 24 * 60 * 60), "month")
    } else {
        (elapsed / (365 * 24 * 60 * 60), "year")
    };
    let plural = if value == 1 { "" } else { "s" };

    if seconds < 0 {
        format!("in {value} {unit}{plural}")
    } else {
        format!("{value} {unit}{plural} ago")
    }
}

fn articles_url(base: &str, filters: &ArticleFilters) -> String {
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    if !filters.feed.is_empty() {
        query.append_pair("feed", &filters.feed);
    }
    if !filters.tag.is_empty() {
        query.append_pair("tag", &filters.tag);
    }
    let status = read_filter_status(filters.read_filter);
    if !status.is_empty() {
        query.append_pair("status", status);
    }
    let query = query.finish();
    if query.is_empty() {
        base.to_owned()
    } else {
        format!("{base}?{query}")
    }
}

fn selected_feed_urls(feeds: &[FeedConfig], filters: &ArticleFilters) -> Vec<String> {
    feeds
        .iter()
        .filter(|feed| filters.feed.is_empty() || feed_name_matches(&feed.name, &filters.feed))
        .filter(|feed| filters.tag.is_empty() || feed.tags.contains(&filters.tag))
        .map(|feed| feed.url.clone())
        .collect()
}

fn feed_name_matches(configured_name: &str, selected_name: &str) -> bool {
    configured_name.to_lowercase() == selected_name.to_lowercase()
}

fn read_filter_from_status(status: Option<&str>) -> ReadFilter {
    match status {
        Some("unread") => ReadFilter::Unread,
        Some("read") => ReadFilter::Read,
        _ => ReadFilter::All,
    }
}

fn read_filter_status(read_filter: ReadFilter) -> &'static str {
    match read_filter {
        ReadFilter::All => "",
        ReadFilter::Unread => "unread",
        ReadFilter::Read => "read",
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

fn build_feeds_panel(
    state: &AppState,
    selected_tag: Option<String>,
) -> Result<FeedsPanel, AppError> {
    Ok(feed_directory(
        state.config.feeds()?,
        selected_tag.unwrap_or_default(),
    ))
}

fn feed_directory(mut feeds: Vec<FeedConfig>, selected_tag: String) -> FeedsPanel {
    let all_active = selected_tag.is_empty();
    let tags = feeds
        .iter()
        .flat_map(|feed| feed.tags.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|name| TagView {
            href: feeds_url("/feeds", &name),
            fragment_href: feeds_url("/feeds/list", &name),
            active: name == selected_tag,
            name,
        })
        .collect();
    feeds.sort_by_key(|feed| feed.name.to_lowercase());
    let feeds = feeds
        .into_iter()
        .filter(|feed| selected_tag.is_empty() || feed.tags.contains(&selected_tag))
        .map(|feed| {
            let posts_href = articles_url(
                "/",
                &ArticleFilters {
                    feed: feed.name.clone(),
                    ..ArticleFilters::default()
                },
            );
            FeedDirectoryView {
                name: feed.name,
                url: feed.url,
                tags: feed.tags,
                posts_href,
            }
        })
        .collect();
    FeedsPanel {
        feeds,
        tags,
        selected_tag,
        all_active,
        all_tags_href: "/feeds".to_owned(),
        all_tags_fragment_href: "/feeds/list".to_owned(),
    }
}

fn feeds_url(base: &str, tag: &str) -> String {
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    if !tag.is_empty() {
        query.append_pair("tag", tag);
    }
    let query = query.finish();
    if query.is_empty() {
        base.to_owned()
    } else {
        format!("{base}?{query}")
    }
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
        selected_feed: panel.selected_feed,
        selected_feed_name: panel.selected_feed_name,
        selected_tag: panel.selected_tag,
        selected_status: panel.selected_status,
        all_feeds_href: panel.all_feeds_href,
        all_feeds_fragment_href: panel.all_feeds_fragment_href,
        all_active: panel.all_active,
        all_tags_href: panel.all_tags_href,
        all_tags_fragment_href: panel.all_tags_fragment_href,
        read_filters: panel.read_filters,
        csrf_token: panel.csrf_token,
        notice: panel.notice,
    })
}

fn render_index(panel: ArticlesPanel) -> Result<Response, AppError> {
    render_template(IndexTemplate {
        articles: panel.articles,
        tags: panel.tags,
        selected_feed: panel.selected_feed,
        selected_feed_name: panel.selected_feed_name,
        selected_tag: panel.selected_tag,
        selected_status: panel.selected_status,
        all_feeds_href: panel.all_feeds_href,
        all_feeds_fragment_href: panel.all_feeds_fragment_href,
        all_active: panel.all_active,
        all_tags_href: panel.all_tags_href,
        all_tags_fragment_href: panel.all_tags_fragment_href,
        read_filters: panel.read_filters,
        csrf_token: panel.csrf_token,
        notice: panel.notice,
        posts_nav_active: true,
        feeds_nav_active: false,
        sources_nav_active: false,
    })
}

fn render_feeds_panel(panel: FeedsPanel) -> Result<Response, AppError> {
    render_template(FeedsPanelTemplate {
        feeds: panel.feeds,
        tags: panel.tags,
        selected_tag: panel.selected_tag,
        all_active: panel.all_active,
        all_tags_href: panel.all_tags_href,
        all_tags_fragment_href: panel.all_tags_fragment_href,
    })
}

fn render_template(template: impl Template) -> Result<Response, AppError> {
    let mut response = Html(template.render()?).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

fn is_htmx(headers: &HeaderMap) -> bool {
    headers
        .get("HX-Request")
        .is_some_and(|value| value == "true")
}

fn verify_csrf(state: &AppState, csrf_token: &str) -> Result<(), AppError> {
    if state.csrf_token == csrf_token {
        Ok(())
    } else {
        Err(AppError::InvalidCsrf)
    }
}

fn verify_user_navigation(headers: &HeaderMap) -> Result<(), AppError> {
    let is_prefetch = ["Purpose", "Sec-Purpose"].iter().any(|name| {
        headers
            .get(*name)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.contains("prefetch"))
    });
    if is_prefetch {
        return Err(AppError::InvalidNavigation);
    }

    match headers.get("Sec-Fetch-User") {
        Some(value) if value != "?1" => Err(AppError::InvalidNavigation),
        _ => Ok(()),
    }
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

async fn navigation_script() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("../static/navigation.js"),
    )
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        error!(%error, "could not install shutdown signal handler");
    }
}

#[derive(Debug, Error)]
enum AppError {
    #[error("article was not found")]
    ArticleNotFound,
    #[error("request verification failed")]
    InvalidCsrf,
    #[error("article opening requires a user navigation")]
    InvalidNavigation,
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Template(#[from] askama::Error),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        if matches!(self, AppError::ArticleNotFound) {
            return (StatusCode::NOT_FOUND, "Article not found.").into_response();
        }
        if matches!(self, AppError::InvalidCsrf | AppError::InvalidNavigation) {
            return (StatusCode::FORBIDDEN, "Request verification failed.").into_response();
        }
        error!(error = %self, "request failed");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "The request could not be completed.",
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::Duration;

    use super::*;

    fn feed(url: &str, name: &str, tags: &[&str]) -> FeedConfig {
        FeedConfig {
            url: url.to_owned(),
            name: name.to_owned(),
            tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
        }
    }

    #[test]
    fn relative_times_use_readable_units_and_handle_future_dates() {
        let now = DateTime::<Utc>::from_timestamp(2_000_000_000, 0).unwrap();

        assert_eq!(relative_time(now - Duration::seconds(59), now), "just now");
        assert_eq!(
            relative_time(now - Duration::minutes(1), now),
            "1 minute ago"
        );
        assert_eq!(
            relative_time(now - Duration::minutes(45), now),
            "45 minutes ago"
        );
        assert_eq!(relative_time(now - Duration::hours(2), now), "2 hours ago");
        assert_eq!(relative_time(now - Duration::days(3), now), "3 days ago");
        assert_eq!(relative_time(now - Duration::weeks(2), now), "2 weeks ago");
        assert_eq!(relative_time(now - Duration::days(90), now), "3 months ago");
        assert_eq!(relative_time(now - Duration::days(730), now), "2 years ago");
        assert_eq!(relative_time(now + Duration::hours(3), now), "in 3 hours");
    }

    #[test]
    fn article_urls_preserve_feed_tag_and_status_filters() {
        let filters = ArticleFilters {
            feed: "Example Daily".to_owned(),
            tag: "local news".to_owned(),
            read_filter: ReadFilter::Unread,
        };

        let url = articles_url("/articles", &filters);
        let parsed = url::Url::parse(&format!("http://localhost{url}")).unwrap();
        let query = parsed
            .query_pairs()
            .into_owned()
            .collect::<BTreeMap<_, _>>();

        assert_eq!(query.get("feed"), Some(&filters.feed));
        assert_eq!(query.get("tag"), Some(&filters.tag));
        assert_eq!(query.get("status").map(String::as_str), Some("unread"));
        assert_eq!(articles_url("/", &ArticleFilters::default()), "/");
    }

    #[test]
    fn feed_and_tag_filters_are_intersected() {
        let feeds = vec![
            feed("https://example.com/news", "News", &["news"]),
            feed("https://example.com/sports", "Sports", &["sports"]),
        ];
        let mut filters = ArticleFilters {
            feed: feeds[0].name.clone(),
            tag: "news".to_owned(),
            ..ArticleFilters::default()
        };

        assert_eq!(
            selected_feed_urls(&feeds, &filters),
            vec![feeds[0].url.clone()]
        );

        filters.tag = "sports".to_owned();
        assert!(selected_feed_urls(&feeds, &filters).is_empty());

        filters.feed = "Missing".to_owned();
        filters.tag.clear();
        assert!(selected_feed_urls(&feeds, &filters).is_empty());
    }

    #[test]
    fn feed_directory_filters_by_tag_and_links_to_posts() {
        let panel = feed_directory(
            vec![
                feed("https://example.com/zebra", "Zebra", &["news"]),
                feed(
                    "https://example.com/alpha",
                    "Alpha Daily",
                    &["news", "technology"],
                ),
                feed("https://example.com/sports", "Sports", &["sports"]),
            ],
            "news".to_owned(),
        );

        assert_eq!(
            panel
                .feeds
                .iter()
                .map(|feed| feed.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Alpha Daily", "Zebra"]
        );
        assert_eq!(
            panel
                .tags
                .iter()
                .map(|tag| tag.name.as_str())
                .collect::<Vec<_>>(),
            vec!["news", "sports", "technology"]
        );
        assert_eq!(panel.feeds[0].posts_href, "/?feed=Alpha+Daily");
        assert!(panel.tags[0].active);
        assert!(!panel.all_active);
    }
}
