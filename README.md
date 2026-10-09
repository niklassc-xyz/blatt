# Local RSS Reader

A small local RSS and Atom reader built with Rust, Axum, Askama, HTMX, and SQLite. It displays Media RSS, image enclosure, and YouTube thumbnails when feeds provide them, tracks read articles, and supports tag and read-status filtering. The interface follows the system light or dark preference using Rosé Pine Dawn and Rosé Pine. It fetches feeds only when you press **Refresh feeds**, then serves the cached articles without contacting external feed servers during normal browsing or filtering.

## Run

Install a current stable Rust toolchain, then run:

```sh
cargo run --release
```

Open <http://127.0.0.1:3000>. Stop the server with `Ctrl+C`.

The application binds only to localhost by default. Set `RSS_READER_ADDR` to use another address:

```sh
RSS_READER_ADDR=127.0.0.1:8080 cargo run --release
```

## Configuration

Feeds can be managed from the **Feeds** page or by editing the XDG-compliant configuration file:

- `$XDG_CONFIG_HOME/rss-reader/feeds.toml`
- `~/.config/rss-reader/feeds.toml` when `XDG_CONFIG_HOME` is unset

The file is created automatically on first start. Its format is:

```toml
[[feeds]]
url = "https://example.com/feed.xml"
name = "Example News"
tags = ["news", "technology"]

[[feeds]]
url = "https://www.youtube.com/feeds/videos.xml?channel_id=UCUyeluBRhGPCW4rPe_UvBZQ"
name = "The Coding Train"
tags = ["video", "programming"]
```

Restart the application after editing this file directly. Changes made through the web interface are persisted immediately with an atomic file replacement.

Cached articles are stored in `$XDG_DATA_HOME/rss-reader/articles.db`, or `~/.local/share/rss-reader/articles.db` when `XDG_DATA_HOME` is unset. Removing a feed through the web interface also removes its cached articles. No periodic background refresh is performed.

## Development

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

HTMX 2.0.8 is vendored at `static/htmx.min.js`, so the application and its enhancements work without a CDN. Every interaction also has a normal link or form fallback and remains usable when JavaScript is disabled.

## License

This project is available under the GNU General Public License version 3 only (`GPL-3.0-only`). The vendored HTMX library uses the BSD 2-Clause License; see `THIRD_PARTY_LICENSES.md`.
