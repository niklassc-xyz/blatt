use std::{
    collections::HashSet,
    env, fs,
    path::{Path, PathBuf},
    sync::Mutex,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct FeedConfig {
    pub url: String,
    pub name: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct ConfigFile {
    #[serde(default)]
    feeds: Vec<FeedConfig>,
}

#[derive(Clone, Debug)]
pub struct AppPaths {
    pub config_file: PathBuf,
    pub database_file: PathBuf,
}

impl AppPaths {
    pub fn discover() -> Result<Self, ConfigError> {
        let home = absolute_environment_path("HOME");
        let config_home = absolute_environment_path("XDG_CONFIG_HOME")
            .or_else(|| home.as_ref().map(|path| path.join(".config")))
            .ok_or(ConfigError::MissingHome)?;
        let data_home = absolute_environment_path("XDG_DATA_HOME")
            .or_else(|| home.map(|path| path.join(".local/share")))
            .ok_or(ConfigError::MissingHome)?;

        Ok(Self {
            config_file: config_home.join("blatt/feeds.toml"),
            database_file: data_home.join("blatt/articles.db"),
        })
    }
}

fn absolute_environment_path(variable: &str) -> Option<PathBuf> {
    env::var_os(variable)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

pub struct ConfigStore {
    path: PathBuf,
    feeds: Mutex<Vec<FeedConfig>>,
}

impl ConfigStore {
    pub fn load(path: impl Into<PathBuf>) -> Result<Self, ConfigError> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut feeds = if path.exists() {
            let contents = fs::read_to_string(&path)?;
            if contents.trim().is_empty() {
                Vec::new()
            } else {
                toml::from_str::<ConfigFile>(&contents)?.feeds
            }
        } else {
            Vec::new()
        };

        normalize_all(&mut feeds)?;
        let store = Self {
            path,
            feeds: Mutex::new(feeds),
        };

        if !store.path.exists() {
            store.persist(&store.feeds()?)?;
        }
        Ok(store)
    }

    pub fn feeds(&self) -> Result<Vec<FeedConfig>, ConfigError> {
        Ok(self.feeds.lock().map_err(|_| ConfigError::Lock)?.clone())
    }

    pub fn add(&self, mut feed: FeedConfig) -> Result<(), ConfigError> {
        normalize_feed(&mut feed)?;
        let mut current = self.feeds.lock().map_err(|_| ConfigError::Lock)?;
        if current.iter().any(|item| item.url == feed.url) {
            return Err(ConfigError::DuplicateUrl);
        }
        if current
            .iter()
            .any(|item| feed_names_equal(&item.name, &feed.name))
        {
            return Err(ConfigError::DuplicateName);
        }

        let mut updated = current.clone();
        updated.push(feed);
        self.persist(&updated)?;
        *current = updated;
        Ok(())
    }

    pub fn update(&self, original_url: &str, mut feed: FeedConfig) -> Result<(), ConfigError> {
        normalize_feed(&mut feed)?;
        let mut current = self.feeds.lock().map_err(|_| ConfigError::Lock)?;
        let index = current
            .iter()
            .position(|item| item.url == original_url)
            .ok_or(ConfigError::NotFound)?;

        if current
            .iter()
            .enumerate()
            .any(|(other_index, item)| other_index != index && item.url == feed.url)
        {
            return Err(ConfigError::DuplicateUrl);
        }
        if current.iter().enumerate().any(|(other_index, item)| {
            other_index != index && feed_names_equal(&item.name, &feed.name)
        }) {
            return Err(ConfigError::DuplicateName);
        }

        let mut updated = current.clone();
        updated[index] = feed;
        self.persist(&updated)?;
        *current = updated;
        Ok(())
    }

    pub fn delete(&self, url: &str) -> Result<(), ConfigError> {
        let mut current = self.feeds.lock().map_err(|_| ConfigError::Lock)?;
        let mut updated = current.clone();
        let previous_len = updated.len();
        updated.retain(|feed| feed.url != url);
        if updated.len() == previous_len {
            return Err(ConfigError::NotFound);
        }

        self.persist(&updated)?;
        *current = updated;
        Ok(())
    }

    fn persist(&self, feeds: &[FeedConfig]) -> Result<(), ConfigError> {
        let contents = toml::to_string_pretty(&ConfigFile {
            feeds: feeds.to_vec(),
        })?;
        let temporary = self.path.with_extension("toml.tmp");
        fs::write(&temporary, contents)?;
        fs::rename(temporary, &self.path)?;
        Ok(())
    }
}

fn normalize_all(feeds: &mut [FeedConfig]) -> Result<(), ConfigError> {
    let mut urls = HashSet::new();
    let mut names = HashSet::new();
    for feed in feeds {
        normalize_feed(feed)?;
        if !urls.insert(feed.url.clone()) {
            return Err(ConfigError::DuplicateUrl);
        }
        if !names.insert(feed.name.to_lowercase()) {
            return Err(ConfigError::DuplicateName);
        }
    }
    Ok(())
}

fn feed_names_equal(left: &str, right: &str) -> bool {
    left.to_lowercase() == right.to_lowercase()
}

fn normalize_feed(feed: &mut FeedConfig) -> Result<(), ConfigError> {
    feed.name = feed.name.trim().to_owned();
    if feed.name.is_empty() {
        return Err(ConfigError::EmptyName);
    }

    let parsed = Url::parse(feed.url.trim()).map_err(|_| ConfigError::InvalidUrl)?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(ConfigError::InvalidUrl);
    }
    feed.url = parsed.to_string();

    let mut seen = HashSet::new();
    feed.tags = feed
        .tags
        .iter()
        .map(|tag| tag.trim())
        .filter(|tag| !tag.is_empty())
        .filter(|tag| seen.insert((*tag).to_owned()))
        .map(str::to_owned)
        .collect();
    feed.tags.sort_by_key(|tag| tag.to_lowercase());
    Ok(())
}

pub fn tags_from_input(input: &str) -> Vec<String> {
    input.split(',').map(str::trim).map(str::to_owned).collect()
}

pub fn config_path_display(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("HOME or the relevant XDG directory is not set")]
    MissingHome,
    #[error("feed name cannot be empty")]
    EmptyName,
    #[error("feed URL must be a valid HTTP or HTTPS URL")]
    InvalidUrl,
    #[error("a feed with that URL already exists")]
    DuplicateUrl,
    #[error("a feed with that name already exists")]
    DuplicateName,
    #[error("feed was not found")]
    NotFound,
    #[error("configuration is currently unavailable")]
    Lock,
    #[error("could not access the configuration file: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid configuration file: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("could not serialize the configuration: {0}")]
    Serialize(#[from] toml::ser::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configuration_round_trips_and_normalizes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("feeds.toml");
        let store = ConfigStore::load(&path).unwrap();

        store
            .add(FeedConfig {
                url: "https://example.com/feed".to_owned(),
                name: " Example ".to_owned(),
                tags: vec![" rust ".to_owned(), "news".to_owned(), "rust".to_owned()],
            })
            .unwrap();

        let reloaded = ConfigStore::load(path).unwrap();
        assert_eq!(
            reloaded.feeds().unwrap(),
            vec![FeedConfig {
                url: "https://example.com/feed".to_owned(),
                name: "Example".to_owned(),
                tags: vec!["news".to_owned(), "rust".to_owned()],
            }]
        );
    }

    #[test]
    fn feed_names_must_be_unique_ignoring_case() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("feeds.toml");
        let store = ConfigStore::load(path).unwrap();

        store
            .add(FeedConfig {
                url: "https://example.com/first".to_owned(),
                name: "Example".to_owned(),
                tags: Vec::new(),
            })
            .unwrap();
        let duplicate = store.add(FeedConfig {
            url: "https://example.com/second".to_owned(),
            name: "example".to_owned(),
            tags: Vec::new(),
        });

        assert!(matches!(duplicate, Err(ConfigError::DuplicateName)));

        store
            .add(FeedConfig {
                url: "https://example.com/other".to_owned(),
                name: "Other".to_owned(),
                tags: Vec::new(),
            })
            .unwrap();
        let duplicate = store.update(
            "https://example.com/other",
            FeedConfig {
                url: "https://example.com/other".to_owned(),
                name: "EXAMPLE".to_owned(),
                tags: Vec::new(),
            },
        );

        assert!(matches!(duplicate, Err(ConfigError::DuplicateName)));
    }

    #[test]
    fn duplicate_feed_names_in_the_configuration_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("feeds.toml");
        fs::write(
            &path,
            r#"
[[feeds]]
url = "https://example.com/first"
name = "Example"

[[feeds]]
url = "https://example.com/second"
name = "example"
"#,
        )
        .unwrap();

        assert!(matches!(
            ConfigStore::load(path),
            Err(ConfigError::DuplicateName)
        ));
    }
}
