use std::{io::Cursor, time::Duration};

use chrono::Utc;
use feed_rs::model::Entry;
use futures_util::{StreamExt, stream};
use reqwest::{Client, Url};

use crate::{
    config::FeedConfig,
    storage::{Article, Storage},
};

const MAX_FEED_BYTES: usize = 10 * 1024 * 1024;
const MAX_CONCURRENT_FETCHES: usize = 8;

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
            .user_agent(concat!("rss-reader/", env!("CARGO_PKG_VERSION")))
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

    async fn fetch(&self, feed: &FeedConfig) -> Result<Vec<Article>, String> {
        let mut response = self
            .client
            .get(&feed.url)
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

        parse_articles(feed, &bytes, &base_url)
    }
}

fn parse_articles(
    feed_config: &FeedConfig,
    bytes: &[u8],
    base_url: &Url,
) -> Result<Vec<Article>, String> {
    let parsed = feed_rs::parser::parse(Cursor::new(bytes)).map_err(|error| error.to_string())?;
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_youtube_atom_entries() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
            <feed xmlns="http://www.w3.org/2005/Atom" xmlns:yt="http://www.youtube.com/xml/schemas/2015">
              <title>Example Channel</title>
              <entry>
                <id>yt:video:abc123</id>
                <yt:videoId>abc123</yt:videoId>
                <title>A useful video</title>
                <link rel="alternate" href="https://www.youtube.com/watch?v=abc123"/>
                <published>2026-01-02T03:04:05+00:00</published>
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
}
