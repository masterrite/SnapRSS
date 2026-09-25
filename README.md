# SnapRSS
An RSS feed reader inspired by QuiteRSS in Rust. For my own use. Written by Claude using Opus 5.5. Icon and name are by me.

## What it does

- **Reading.** Three-pane Classic layout or a one-column Newspaper layout,
  relaxed or compact rows, and eight themes. Articles show the full page text,
  extracted in the background and cleaned of scripts and clutter; anything
  that needs a real browser opens in yours.
- **Organising.** Folders nested to any depth, arranged by drag and drop.
  Unread, All, Starred and Deleted views. Colored labels.
- **Filters.** Rules that act on new articles as they arrive: mark read,
  star, label, delete, play a sound or notify.
- **Housekeeping.** Retention rules, global or per feed, that move old
  articles to Deleted, where they can still be restored. Unread, starred and
  labelled articles can be protected from them. Backups from the app menu;
  compacting and clearing the article cache on the Storage page.
- **Updating.** Each feed polls on its own interval and asks the server
  whether anything changed before downloading, so an unchanged feed costs
  almost nothing. A failing feed backs off and is marked in the tree.
- **Your way.** Four toolbars you can rearrange, resizable panes, keyboard
  shortcuts, undo, the system tray, start with Windows, and updates that
  install themselves.

## Installing

Download the `-setup.exe` installer from the repository's Releases page and
run it. SnapRSS needs WebView2, which comes with Windows 11 and current
Windows 10.

To get updates, enter the repository as `owner/name` under
**Settings → Updates**. SnapRSS then checks three minutes after starting and
once a day, and shows a bar when a new version is ready. Updates are signed,
and SnapRSS refuses any download whose signature doesn't match.

## Moving from QuiteRSS

Click **Import** and choose either file:

- **QuiteRSS's database** (`feeds.db` in its profile folder) brings over
  everything: feeds, folders, articles, read and starred state, labels,
  filters and saved passwords.
- **An OPML file** (QuiteRSS: Feeds → Export Feeds) brings subscriptions and
  folders only.

The file type is detected from its contents, not its name. Importing only
adds: feeds you already have are skipped, so importing the same file twice
changes nothing.

**Export** writes OPML 2.0 with your subscriptions and folders.

## Keyboard

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

## Where your data lives

    %APPDATA%\com.snaprss.app\snaprss.db

Theme, density, layout, toolbar arrangement and pane sizes are per-machine
preferences and are kept by the window itself, not in the database.

## Building from source

You need Rust, the MSVC build tools and WebView2 on Windows. The Linux and
macOS dependencies are listed in `crates/app/README.md`; the app is built
there for development only.

    cargo install tauri-cli --version "^2.0.0" --locked
    cd crates/app
    cargo tauri dev        # run it
    cargo tauri build      # installer in target/release/bundle/nsis/

## Project layout

    crates/core/      database, import, OPML, retention, filters
    crates/fetch/     downloading feeds, scheduling, saving new articles
    crates/article/   extracting and cleaning article text
    crates/app/       the window: Tauri shell, ui/ frontend, ui-test/ checks

The first three have no window and build and test anywhere. `crates/app/README.md`
covers the app in depth: commands, background updating, the sidebar, toolbars,
settings, the tray, icons and security.

## About QuiteRSS

SnapRSS reads QuiteRSS's database format to import from it. It contains no
QuiteRSS code, and its own database schema is its own.

## License

Apache-2.0. See `LICENSE` and `NOTICE`.