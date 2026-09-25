<p align="center">
<img width="1440" height="930" alt="Snipaste_2026-09-24_20-21-55" src="https://github.com/user-attachments/assets/90c18bcf-f998-4ddb-8474-47a0ed1132ab"/>
</p>

# SnapRSS
An RSS feed reader inspired by QuiteRSS in Rust and uses Tauri frontend. For my own use. Written by Claude using Opus 5.5. Icon and name are by me.

I've been using QuiteRSS for years. But it has fell out of maintenance and the interface has become outdated and clunky. Apparently it's quite a spaghetti code, too, making migration from Qt 5 to modern Qt 6 hard, according to discussions in its Issues page. Thanks to the advent of agentic AI, I now have the tools to realize my ideas without a developer or pay my friend way more than I could afford to build something he definitely won't have time to maintain. So here it is, QuiteRSS rewritten from scratch in Rust.

## Features

It's an RSS feed reader... What else do you want? Check out the settings to find out what else this thing can do.

I'm too lazy to write a full thing but here's a short list:
- themes
- customizable toolbars
- labels, rules, and filters
- keyboard shortcuts
- automatic updater

------- Everything below are written by Claude and trimmed by me -------

## Installing

Download the `-setup.exe` installer from the repository's Releases page and
run.

SnapRSS checks for updates three minutes after starting and
once a day. It shows a bar when a new version is ready.

## Moving from QuiteRSS

Click **Import** and choose either file:

- **QuiteRSS's database** (`feeds.db` in its profile folder) brings over
  everything: feeds, folders, articles, read and starred state, labels,
  filters and saved passwords.
- **An OPML file** (QuiteRSS: Feeds → Export Feeds) brings subscriptions and
  folders only.

**Export** writes OPML 2.0 with your subscriptions and folders.

## Keyboard shortcuts

| Key | Action |
|---|---|
| `J` / `K` | Next / previous article |
| `Shift+J` / `Shift+K` | Extend the selection |
| `S` | Star |
| `M` | Toggle read |
| `Shift+M` | Mark all as read |
| `L` | Labels |
| `B` or `Enter` | Open in browser |
| `Delete` | Delete the selected articles, or the selected feed when the tree has focus |
| `Ctrl+Z` | Undo the last delete, restore or mark-all-read |
| `Ctrl+A` / `Escape` | Select all / clear the selection |
| `/` | Search |
| `F5` | Update all feeds |
| `Ctrl+,` | Settings |

## Database path

    %APPDATA%\com.snaprss.app\snaprss.db

Theme, density, layout, toolbar arrangement and pane sizes are per-machine
preferences and are kept by the window itself, not in the database.

## Building from source

You need Rust, the MSVC build tools and WebView2 on Windows.

    cargo install tauri-cli --version "^2.0.0" --locked
    cd crates/app
    cargo tauri dev        # run it
    cargo tauri build      # installer in target/release/bundle/nsis/

## Project layout

    crates/core/      database, import, OPML, retention, filters
    crates/fetch/     downloading feeds, scheduling, saving new articles
    crates/article/   extracting and cleaning article text
    crates/app/       the window: Tauri shell, ui/ frontend, ui-test/ checks

## License

Apache-2.0. See `LICENSE` and `NOTICE`.
