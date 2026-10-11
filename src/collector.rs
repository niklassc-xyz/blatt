use std::{io::Cursor, time::Duration};

use chrono::Utc;
use feed_rs::model::{Entry, Feed};
use futures_util::{StreamExt, stream};
use reqwest::{Client, Url};

use crate::{
    config::FeedConfig,
    storage::{Article, Storage},
};

const MAX_FEED_BYTES: usize = 10 * 1024 * 1024;
const MAX_CONCURRENT_FETCHES: usize = 8;
const YOUTUBE_FEED_URL: &str = "https://www.youtube.com/feeds/videos.xml";

pub fn is_youtube_channel_url(value: &str) -> bool {
    parse_youtube_channel_url(value).is_some()
}

#[derive(Clone)]
pub struct FeedCollector {
    client: Client,
}

#[derive(Debug, Default)]
pub struct RefreshReport {
    pub successful_feeds: usize,
    pub articles_seen: usize,
    pub failures: Vec<FeedFailure>,
}

#[derive(Debug)]
pub struct FeedFailure {
    pub name: String,
    pub reason: String,
}

impl FeedCollector {
    pub fn new() -> Result<Self, reqwest::Error> {
        let client = Client::builder()
            .user_agent(concat!("blatt/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(20))
            .build()?;
        Ok(Self { client })
    }

    pub async fn refresh(&self, feeds: Vec<FeedConfig>, storage: &Storage) -> RefreshReport {
        let results = stream::iter(feeds.into_iter().map(|feed| {
            let collector = self.clone();
            async move {
                let result = collector.fetch(&feed).await;
                (feed, result)
            }
        }))
        .buffer_unordered(MAX_CONCURRENT_FETCHES)
        .collect::<Vec<_>>()
        .await;

        let mut report = RefreshReport::default();
        for (feed, result) in results {
            match result {
                Ok(articles) => {
                    let article_count = articles.len();
                    match storage.upsert_articles(&articles).await {
                        Ok(()) => {
                            report.successful_feeds += 1;
                            report.articles_seen += article_count;
                        }
                        Err(error) => report.failures.push(FeedFailure {
                            name: feed.name,
                            reason: format!("could not cache articles: {error}"),
                        }),
                    }
                }
                Err(reason) => report.failures.push(FeedFailure {
                    name: feed.name,
                    reason,
                }),
            }
        }
        report
    }

    pub async fn fetch_title(&self, url: &str) -> Result<String, String> {
        let (bytes, base_url) = self.download(url).await?;
        parse_title(&bytes, &base_url)
    }

    pub async fn resolve_youtube_feed_url(&self, url: &str) -> Result<String, String> {
        let channel_url = parse_youtube_channel_url(url)
            .ok_or_else(|| "URL is not a recognized YouTube channel page".to_owned())?;
        if let Some(channel_id) = youtube_channel_id(&channel_url) {
            return Ok(youtube_feed_url(&channel_id));
        }

        let (bytes, _) = self.download(channel_url.as_str()).await?;
        youtube_feed_url_from_page(&bytes)
            .ok_or_else(|| "YouTube did not advertise a feed for that channel page".to_owned())
    }

    async fn fetch(&self, feed: &FeedConfig) -> Result<Vec<Article>, String> {
        let (bytes, base_url) = self.download(&feed.url).await?;
        parse_articles(feed, &bytes, &base_url)
    }

    async fn download(&self, url: &str) -> Result<(Vec<u8>, Url), String> {
        let url = Url::parse(url.trim())
            .ok()
            .filter(|url| matches!(url.scheme(), "http" | "https"))
            .ok_or_else(|| "feed URL must be a valid HTTP or HTTPS URL".to_owned())?;
        let mut response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|error| error.to_string())?
            .error_for_status()
            .map_err(|error| error.to_string())?;
        let base_url = response.url().clone();

        if response
            .content_length()
            .is_some_and(|size| size > MAX_FEED_BYTES as u64)
        {
            return Err("feed is larger than 10 MiB".to_owned());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|error| error.to_string())? {
            if bytes.len().saturating_add(chunk.len()) > MAX_FEED_BYTES {
                return Err("feed is larger than 10 MiB".to_owned());
            }
            bytes.extend_from_slice(&chunk);
        }

        Ok((bytes, base_url))
    }
}

fn parse_youtube_channel_url(value: &str) -> Option<Url> {
    let url = Url::parse(value.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let host = url.host_str()?;
    if host != "youtube.com" && !host.ends_with(".youtube.com") {
        return None;
    }

    let mut segments = url.path_segments()?;
    let first = segments.next()?;
    let second = segments.next();
    let is_channel = (first.starts_with('@') && first.len() > 1)
        || (matches!(first, "channel" | "c" | "user")
            && second.is_some_and(|value| !value.is_empty()));
    is_channel.then_some(url)
}

fn youtube_channel_id(url: &Url) -> Option<String> {
    let mut segments = url.path_segments()?;
    (segments.next()? == "channel")
        .then(|| segments.next().map(str::to_owned))
        .flatten()
}

fn youtube_feed_url(channel_id: &str) -> String {
    let mut url = Url::parse(YOUTUBE_FEED_URL).expect("YouTube feed URL must be valid");
    url.query_pairs_mut().append_pair("channel_id", channel_id);
    url.to_string()
}

fn youtube_feed_url_from_page(bytes: &[u8]) -> Option<String> {
    let page = String::from_utf8_lossy(bytes);
    [
        "youtube.com/feeds/videos.xml?channel_id=",
        r"youtube.com\/feeds\/videos.xml?channel_id=",
    ]
    .into_iter()
    .find_map(|marker| {
        let remainder = page.split_once(marker)?.1;
        let channel_id = remainder
            .chars()
            .take_while(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
            })
            .collect::<String>();
        (!channel_id.is_empty()).then(|| youtube_feed_url(&channel_id))
    })
}

fn parse_feed(bytes: &[u8], base_url: &Url) -> Result<Feed, String> {
    feed_rs::parser::Builder::new()
        .base_uri(Some(base_url.as_str()))
        .build()
        .parse(Cursor::new(bytes))
        .map_err(|error| error.to_string())
}

fn parse_title(bytes: &[u8], base_url: &Url) -> Result<String, String> {
    parse_feed(bytes, base_url)?
        .title
        .map(|title| title.content.trim().to_owned())
        .filter(|title| !title.is_empty())
        .ok_or_else(|| "feed does not have a title".to_owned())
}

fn parse_articles(
    feed_config: &FeedConfig,
    bytes: &[u8],
    base_url: &Url,
) -> Result<Vec<Article>, String> {
    let parsed = parse_feed(bytes, base_url)?;
    let fetched_at = Utc::now().timestamp();

    Ok(parsed
        .entries
        .into_iter()
        .filter_map(|entry| article_from_entry(entry, feed_config, base_url, fetched_at))
        .collect())
}

fn article_from_entry(
    entry: Entry,
    feed: &FeedConfig,
    base_url: &Url,
    fetched_at: i64,
) -> Option<Article> {
    let thumbnail_url = thumbnail_from_entry(&entry, base_url);
    let title = entry.title?.content.trim().to_owned();
    if title.is_empty() {
        return None;
    }

    let href = entry
        .links
        .iter()
        .find(|link| {
            link.rel
                .as_deref()
                .is_none_or(|relation| relation == "alternate")
        })
        .or_else(|| entry.links.first())?
        .href
        .as_str();
    let mut article_url = Url::parse(href).or_else(|_| base_url.join(href)).ok()?;
    if !matches!(article_url.scheme(), "http" | "https") {
        return None;
    }
    article_url.set_fragment(None);
    let article_url = article_url.to_string();
    let published = entry.published.or(entry.updated);
    let published_at = published.map_or(fetched_at, |date| date.timestamp());

    Some(Article {
        dedupe_key: article_url.clone(),
        feed_url: feed.url.clone(),
        title,
        url: article_url,
        source_name: feed.name.clone(),
        published_at,
        fetched_at,
        date_is_fallback: published.is_none(),
        thumbnail_url,
    })
}

fn thumbnail_from_entry(entry: &Entry, base_url: &Url) -> Option<String> {
    entry
        .media
        .iter()
        .flat_map(|media| media.thumbnails.iter())
        .filter_map(|thumbnail| {
            let url = web_url(&thumbnail.image.uri, base_url)?;
            let area = u64::from(thumbnail.image.width.unwrap_or_default())
                * u64::from(thumbnail.image.height.unwrap_or_default());
            Some((area, url))
        })
        .max_by_key(|(area, _)| *area)
        .map(|(_, url)| url)
        .or_else(|| {
            entry
                .media
                .iter()
                .flat_map(|media| media.content.iter())
                .filter_map(|content| {
                    let url = content.url.as_ref()?;
                    if !matches!(url.scheme(), "http" | "https") {
                        return None;
                    }
                    if !is_image_url(content.content_type.as_ref().map(AsRef::as_ref), url) {
                        return None;
                    }
                    let area = u64::from(content.width.unwrap_or_default())
                        * u64::from(content.height.unwrap_or_default());
                    Some((area, url.to_string()))
                })
                .max_by_key(|(area, _)| *area)
                .map(|(_, url)| url)
        })
        .or_else(|| {
            entry.links.iter().find_map(|link| {
                if link.rel.as_deref() != Some("enclosure") {
                    return None;
                }
                let url = Url::parse(&link.href)
                    .or_else(|_| base_url.join(&link.href))
                    .ok()?;
                if !matches!(url.scheme(), "http" | "https") {
                    return None;
                }
                is_image_url(link.media_type.as_deref(), &url).then(|| url.to_string())
            })
        })
}

fn is_image_url(media_type: Option<&str>, url: &Url) -> bool {
    match media_type {
        Some(media_type) => media_type.trim().to_ascii_lowercase().starts_with("image/"),
        None => url
            .path_segments()
            .and_then(Iterator::last)
            .and_then(|name| name.rsplit_once('.').map(|(_, extension)| extension))
            .is_some_and(|extension| {
                matches!(
                    extension.to_ascii_lowercase().as_str(),
                    "avif" | "gif" | "jpeg" | "jpg" | "png" | "webp"
                )
            }),
    }
}

fn web_url(value: &str, base_url: &Url) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value.starts_with('#') {
        return None;
    }
    let mut url = Url::parse(value).or_else(|_| base_url.join(value)).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    url.set_fragment(None);
    Some(url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_trims_feed_title() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
            <rss version="2.0">
              <channel>
                <title> Example News </title>
                <link>https://example.com/</link>
                <description>News</description>
              </channel>
            </rss>"#;
        let base_url = Url::parse("https://example.com/feed.xml").unwrap();

        assert_eq!(parse_title(xml, &base_url).unwrap(), "Example News");
    }

    #[test]
    fn recognizes_youtube_channel_pages_but_not_video_or_feed_urls() {
        assert!(is_youtube_channel_url(
            "https://www.youtube.com/@example/videos"
        ));
        assert!(is_youtube_channel_url("https://youtube.com/channel/UC123"));
        assert!(is_youtube_channel_url("https://m.youtube.com/user/example"));
        assert!(!is_youtube_channel_url(
            "https://www.youtube.com/watch?v=example"
        ));
        assert!(!is_youtube_channel_url(
            "https://www.youtube.com/feeds/videos.xml?channel_id=UC123"
        ));
        assert!(!is_youtube_channel_url("https://notyoutube.com/@example"));
    }

    #[test]
    fn converts_channel_ids_and_discovers_feed_urls_in_channel_pages() {
        let channel_url = Url::parse("https://www.youtube.com/channel/UC123/videos").unwrap();
        assert_eq!(
            youtube_feed_url(&youtube_channel_id(&channel_url).unwrap()),
            "https://www.youtube.com/feeds/videos.xml?channel_id=UC123"
        );

        let page = br#"<link rel="alternate" type="application/rss+xml"
            href="https://www.youtube.com/feeds/videos.xml?channel_id=UC456">"#;
        assert_eq!(
            youtube_feed_url_from_page(page).as_deref(),
            Some("https://www.youtube.com/feeds/videos.xml?channel_id=UC456")
        );
    }

    #[test]
    fn parses_youtube_atom_entries() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
            <feed xmlns="http://www.w3.org/2005/Atom"
                  xmlns:yt="http://www.youtube.com/xml/schemas/2015"
                  xmlns:media="http://search.yahoo.com/mrss/">
              <title>Example Channel</title>
              <entry>
                <id>yt:video:abc123</id>
                <yt:videoId>abc123</yt:videoId>
                <title>A useful video</title>
                <link rel="alternate" href="https://www.youtube.com/watch?v=abc123"/>
                <published>2026-01-02T03:04:05+00:00</published>
                <media:group>
                  <media:thumbnail url="https://i.ytimg.com/vi/abc123/hqdefault.jpg" width="480" height="360"/>
                </media:group>
              </entry>
            </feed>"#;
        let feed = FeedConfig {
            url: "https://www.youtube.com/feeds/videos.xml?channel_id=example".to_owned(),
            name: "Example Channel".to_owned(),
            tags: vec!["video".to_owned()],
        };

        let base_url = Url::parse(&feed.url).unwrap();
        let articles = parse_articles(&feed, xml, &base_url).unwrap();
        assert_eq!(articles.len(), 1);
        assert_eq!(articles[0].title, "A useful video");
        assert_eq!(articles[0].url, "https://www.youtube.com/watch?v=abc123");
        assert_eq!(articles[0].source_name, "Example Channel");
        assert_eq!(
            articles[0].thumbnail_url.as_deref(),
            Some("https://i.ytimg.com/vi/abc123/hqdefault.jpg")
        );
    }

    #[test]
    fn resolves_implicit_alternate_links_against_final_feed_url() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
            <feed xmlns="http://www.w3.org/2005/Atom">
              <title>Redirected Feed</title>
              <entry>
                <id>post-1</id>
                <title>Relative article</title>
                <link rel="self" href="feed.xml"/>
                <link href="/articles/one"/>
                <published>2026-01-02T03:04:05+00:00</published>
              </entry>
            </feed>"#;
        let feed = FeedConfig {
            url: "https://old.example/feed.xml".to_owned(),
            name: "Redirected Feed".to_owned(),
            tags: Vec::new(),
        };
        let final_url = Url::parse("https://new.example/feeds/current.xml").unwrap();

        let articles = parse_articles(&feed, xml, &final_url).unwrap();
        assert_eq!(articles.len(), 1);
        assert_eq!(articles[0].url, "https://new.example/articles/one");
    }

    #[test]
    fn uses_image_enclosures_as_thumbnails() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
            <rss version="2.0">
              <channel>
                <title>Photo Feed</title>
                <link>https://example.com/</link>
                <description>Photos</description>
                <item>
                  <title>A photographed story</title>
                  <link>https://example.com/story</link>
                  <enclosure url="images/story.jpg" type="image/jpeg" length="12000"/>
                </item>
              </channel>
            </rss>"#;
        let feed = FeedConfig {
            url: "https://example.com/feed.xml".to_owned(),
            name: "Photo Feed".to_owned(),
            tags: Vec::new(),
        };
        let base_url = Url::parse("https://cdn.example.com/redirected/feed.xml").unwrap();

        let articles = parse_articles(&feed, xml, &base_url).unwrap();
        assert_eq!(articles.len(), 1);
        assert_eq!(
            articles[0].thumbnail_url.as_deref(),
            Some("https://cdn.example.com/redirected/images/story.jpg")
        );
    }
}
