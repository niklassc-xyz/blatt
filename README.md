# Blatt

Blatt is a small local RSS and Atom reader built with Rust, Axum, Askama, HTMX, and SQLite. It displays Media RSS, image enclosure, and YouTube thumbnails when feeds provide them, tracks read articles, and supports feed, tag, and read-status filtering. The interface follows the system light or dark preference using Rosé Pine Dawn and Rosé Pine. It fetches feeds only when you press **Refresh feeds**, then serves the cached articles without contacting external feed servers during normal browsing or filtering.

## Install

### AUR

```sh
cd packaging/aur
makepkg -si
systemctl --user enable --now blatt.service
```

Open <http://127.0.0.1:3000> after installation.

## Configuration

Feeds can be browsed by name and tag from the **Feeds** page. Select one to show only its posts. Subscriptions can be managed from the **Sources** page or by editing the XDG-compliant configuration file:

- `$XDG_CONFIG_HOME/blatt/feeds.toml`
- `~/.config/blatt/feeds.toml` when `XDG_CONFIG_HOME` is unset

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

Feed names and URLs must be unique. Name comparisons are case-insensitive.

Restart the application after editing this file directly. Changes made through the web interface are persisted immediately with an atomic file replacement.

Cached articles are stored in `$XDG_DATA_HOME/blatt/articles.db`, or `~/.local/share/blatt/articles.db` when `XDG_DATA_HOME` is unset. Removing a feed through the web interface also removes its cached articles. No periodic background refresh is performed.

## Development

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

HTMX 2.0.8 is vendored at `static/htmx.min.js`, so the application and its enhancements work without a CDN. Every interaction also has a normal link or form fallback and remains usable when JavaScript is disabled.

## License

This project is available under the GNU General Public License version 3 only (`GPL-3.0-only`). The vendored HTMX library uses the BSD 2-Clause License; see `THIRD_PARTY_LICENSES.md`.
