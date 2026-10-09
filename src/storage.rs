use std::path::Path;

use sqlx::{QueryBuilder, Sqlite, SqlitePool, sqlite::SqlitePoolOptions};

#[derive(Clone)]
pub struct Storage {
    pool: SqlitePool,
}

#[derive(Clone, Debug)]
pub struct Article {
    pub dedupe_key: String,
    pub feed_url: String,
    pub title: String,
    pub url: String,
    pub source_name: String,
    pub published_at: i64,
    pub fetched_at: i64,
    pub date_is_fallback: bool,
    pub thumbnail_url: Option<String>,
}

#[derive(Clone, Debug)]
pub struct StoredArticle {
    pub title: String,
    pub url: String,
    pub source_name: String,
    pub published_at: i64,
    pub thumbnail_url: Option<String>,
}

impl Storage {
    pub async fn open(path: &Path) -> Result<Self, sqlx::Error> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(sqlx::Error::Io)?;
        }

        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await?;
        let storage = Self { pool };
        storage.initialize().await?;
        Ok(storage)
    }

    async fn initialize(&self) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS articles (
                dedupe_key TEXT NOT NULL,
                feed_url TEXT NOT NULL,
                title TEXT NOT NULL,
                url TEXT NOT NULL,
                source_name TEXT NOT NULL,
                published_at INTEGER NOT NULL,
                fetched_at INTEGER NOT NULL,
                thumbnail_url TEXT,
                PRIMARY KEY (feed_url, dedupe_key)
            )
            "#,
        )
        .execute(&self.pool)
        .await?;
        let has_thumbnail_column: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('articles') WHERE name = 'thumbnail_url')",
        )
        .fetch_one(&self.pool)
        .await?;
        if !has_thumbnail_column {
            sqlx::query("ALTER TABLE articles ADD COLUMN thumbnail_url TEXT")
                .execute(&self.pool)
                .await?;
        }
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS articles_feed_date ON articles(feed_url, published_at DESC)",
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn upsert_articles(&self, articles: &[Article]) -> Result<(), sqlx::Error> {
        let mut transaction = self.pool.begin().await?;
        for article in articles {
            sqlx::query(
                r#"
                INSERT INTO articles (
                    dedupe_key, feed_url, title, url, source_name, published_at, fetched_at,
                    thumbnail_url
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(feed_url, dedupe_key) DO UPDATE SET
                    title = excluded.title,
                    url = excluded.url,
                    source_name = excluded.source_name,
                    published_at = CASE
                        WHEN ? THEN articles.published_at
                        ELSE excluded.published_at
                    END,
                    fetched_at = excluded.fetched_at,
                    thumbnail_url = COALESCE(excluded.thumbnail_url, articles.thumbnail_url)
                "#,
            )
            .bind(&article.dedupe_key)
            .bind(&article.feed_url)
            .bind(&article.title)
            .bind(&article.url)
            .bind(&article.source_name)
            .bind(article.published_at)
            .bind(article.fetched_at)
            .bind(&article.thumbnail_url)
            .bind(article.date_is_fallback)
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    pub async fn list_articles(
        &self,
        feed_urls: &[String],
    ) -> Result<Vec<StoredArticle>, sqlx::Error> {
        if feed_urls.is_empty() {
            return Ok(Vec::new());
        }

        let mut query = QueryBuilder::<Sqlite>::new(
            r#"
            SELECT title, url, source_name, published_at, selected_thumbnail_url FROM (
                SELECT title, url, source_name, published_at, thumbnail_url,
                    FIRST_VALUE(thumbnail_url) OVER (
                        PARTITION BY url ORDER BY
                            (thumbnail_url IS NULL) ASC,
                            published_at DESC,
                            feed_url ASC
                    ) AS selected_thumbnail_url,
                    ROW_NUMBER() OVER (
                        PARTITION BY url ORDER BY
                            published_at DESC,
                            feed_url ASC
                    ) AS result_order
                FROM articles WHERE feed_url IN (
            "#,
        );
        let mut separated = query.separated(", ");
        for url in feed_urls {
            separated.push_bind(url);
        }
        separated
            .push_unseparated(") ) WHERE result_order = 1 ORDER BY published_at DESC, title ASC");

        let rows = query
            .build_query_as::<(String, String, String, i64, Option<String>)>()
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .into_iter()
            .map(
                |(title, url, source_name, published_at, thumbnail_url)| StoredArticle {
                    title,
                    url,
                    source_name,
                    published_at,
                    thumbnail_url,
                },
            )
            .collect())
    }

    pub async fn delete_feed(&self, feed_url: &str) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM articles WHERE feed_url = ?")
            .bind(feed_url)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn update_source_name(
        &self,
        feed_url: &str,
        source_name: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE articles SET source_name = ? WHERE feed_url = ?")
            .bind(source_name)
            .bind(feed_url)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn upsert_deduplicates_by_article_url() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::open(&directory.path().join("articles.db"))
            .await
            .unwrap();
        let mut article = Article {
            dedupe_key: "https://example.com/article".to_owned(),
            feed_url: "https://example.com/feed".to_owned(),
            title: "Original".to_owned(),
            url: "https://example.com/article".to_owned(),
            source_name: "Example".to_owned(),
            published_at: 1,
            fetched_at: 1,
            date_is_fallback: true,
            thumbnail_url: Some("https://example.com/original.jpg".to_owned()),
        };
        storage.upsert_articles(&[article.clone()]).await.unwrap();
        article.title = "Updated".to_owned();
        article.published_at = 2;
        article.fetched_at = 2;
        article.thumbnail_url = Some("https://example.com/updated.jpg".to_owned());
        storage.upsert_articles(&[article]).await.unwrap();

        let articles = storage
            .list_articles(&["https://example.com/feed".to_owned()])
            .await
            .unwrap();
        assert_eq!(articles.len(), 1);
        assert_eq!(articles[0].title, "Updated");
        assert_eq!(articles[0].published_at, 1);
        assert_eq!(
            articles[0].thumbnail_url.as_deref(),
            Some("https://example.com/updated.jpg")
        );
    }

    #[tokio::test]
    async fn shared_article_is_visible_through_either_feed_without_duplicates() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::open(&directory.path().join("articles.db"))
            .await
            .unwrap();
        let first = Article {
            dedupe_key: "https://example.com/shared".to_owned(),
            feed_url: "https://example.com/first.xml".to_owned(),
            title: "Shared article".to_owned(),
            url: "https://example.com/shared".to_owned(),
            source_name: "First".to_owned(),
            published_at: 1,
            fetched_at: 1,
            date_is_fallback: false,
            thumbnail_url: Some("https://example.com/shared.jpg".to_owned()),
        };
        let mut second = first.clone();
        second.feed_url = "https://example.com/second.xml".to_owned();
        second.source_name = "Second".to_owned();
        second.title = "Shared article updated".to_owned();
        second.published_at = 2;
        second.thumbnail_url = None;
        storage.upsert_articles(&[first, second]).await.unwrap();

        let all = storage
            .list_articles(&[
                "https://example.com/first.xml".to_owned(),
                "https://example.com/second.xml".to_owned(),
            ])
            .await
            .unwrap();
        let second_only = storage
            .list_articles(&["https://example.com/second.xml".to_owned()])
            .await
            .unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].title, "Shared article updated");
        assert_eq!(all[0].source_name, "Second");
        assert_eq!(
            all[0].thumbnail_url.as_deref(),
            Some("https://example.com/shared.jpg")
        );
        assert_eq!(second_only.len(), 1);
        assert_eq!(second_only[0].source_name, "Second");
    }

    #[tokio::test]
    async fn migrates_an_existing_cache_for_thumbnails() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("articles.db");
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query(
            r#"
            CREATE TABLE articles (
                dedupe_key TEXT NOT NULL,
                feed_url TEXT NOT NULL,
                title TEXT NOT NULL,
                url TEXT NOT NULL,
                source_name TEXT NOT NULL,
                published_at INTEGER NOT NULL,
                fetched_at INTEGER NOT NULL,
                PRIMARY KEY (feed_url, dedupe_key)
            )
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;

        let storage = Storage::open(&path).await.unwrap();
        let has_thumbnail_column: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('articles') WHERE name = 'thumbnail_url')",
        )
        .fetch_one(&storage.pool)
        .await
        .unwrap();
        assert!(has_thumbnail_column);
    }
}
