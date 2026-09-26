# snaprss-app

The Tauri shell. The logic lives in `snaprss-core`, `snaprss-fetch` and
`snaprss-article`, which build and test without a window. This crate validates
input, calls into those, and maps errors.

## Running

    cargo install tauri-cli --version "^2.0.0" --locked
    cd crates/app && cargo tauri dev

## Building

    cd crates/app && cargo tauri build

SnapRSS is built for Windows only. The bundle is the NSIS installer, in
`target/release/bundle/nsis/`, plus its `.sig` for the updater (the build
signs it when `TAURI_SIGNING_PRIVATE_KEY` is set).

## Releases and updates

`.github/workflows/release.yml` builds, signs and publishes a GitHub release
with a `latest.json`. Nothing needs editing first. Either:

- On the website: **Actions → Release → Run workflow**, type the version
  (`1.0.1`) and press **Run workflow**. The build creates the `v1.0.1` tag
  and the release on the latest commit of the chosen branch. A version that
  already exists, or one not written like `1.0.1`, stops it straight away.
- From a terminal: `git tag v1.0.1` and `git push origin v1.0.1`.

The version is written into the workspace `Cargo.toml` and
`tauri.conf.json` during the build, so the version committed in the
repository only affects local builds.

Publishing a release by hand on the Releases page with a new tag also starts
the build, which adds the installer and `latest.json` to that release and
keeps your title and notes. Until the build finishes (about 15 minutes) the
release has no `latest.json`, so update checks in that window say to try
again later. Saving it as a draft does not create the tag, so nothing is
built until it is published.

The repository needs one secret, `TAURI_SIGNING_PRIVATE_KEY`,
holding the private key that matches the public key in `tauri.conf.json`
(`plugins.updater.pubkey`). The app only installs updates signed with it; a
lost key means installed copies can no longer update themselves and need the
next installer run by hand.

The app reads its release location from the `updates.repo` setting
(Settings → Updates): `owner/name` for a GitHub repository, or a full URL to
a `latest.json`. It checks three minutes after starting and then daily, and
from Help → Check for updates. A found update shows a bar; installing
downloads it, verifies the signature, and on Windows runs the installer
passively, which closes and restarts the app.

Verified end to end on Linux against a local server: a check found the newer
version, the download was verified and installed over the executable, and the
app restarted into it; a tampered download was refused with the app left as
it was. The Windows installer step itself runs only on Windows.

## System dependencies

**Windows.** MSVC build tools, and WebView2 at runtime. WebView2 ships with
Windows 11 and recent Windows 10; the bundler can embed the bootstrapper for
older machines.

**Linux.**

    libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev
    librsvg2-dev libsoup-3.0-dev

**macOS.** Xcode command line tools.

## Where things live at runtime

One SQLite file in the platform app-data directory:

    Windows  %APPDATA%\com.snaprss.app\snaprss.db
    Linux    ~/.local/share/com.snaprss.app/snaprss.db
    macOS    ~/Library/Application Support/com.snaprss.app/snaprss.db

Theme and density are per-machine preferences with no reason to be in the
database, so they live in the webview's localStorage.

## Commands

Grouped by what they touch:

    reading   feed_tree  news_list  article  counts  labels
    writing   set_read  set_starred  set_deleted  mark_scope_read
    feeds     add_feed  remove_feed  rename_node  add_folder  set_reading_mode
              feed_settings  save_feed_settings  apply_settings_to_folder
              move_node
    labels    save_label  delete_label  set_label  clear_labels  reorder_label
    filters   filters  filter_vocabulary  save_filter  delete_filter
              set_filter_enabled  reorder_filter  apply_filters_now
    updating  update_all  update_feed_now
    import    import_file  export_opml
    settings  get_settings  set_settings
    storage   db_stats  run_cleanup  purge_deleted  vacuum_db
              clear_article_cache  backup_db

`filter_vocabulary` returns the fields, the operators legal for each, and the
action names, so the editor's dropdowns cannot drift from what the engine
accepts. `save_filter` rejects a regex that will not compile.

`move_node` takes `into`, `before` or `after` and renumbers the whole sibling
list rather than nudging one row; sparse or duplicated `row_to_parent` values
make a tree drift. It refuses to put a folder inside its own subtree, which
would turn the tree into a ring.

`news_list` takes a scope string: `all`, `unread`, `starred`, `deleted`,
`feed:<id>`, `folder:<id>` (recursive, any depth) or `label:<id>`.

The database sits behind a `tokio::sync::Mutex`, not a `std` one: these are
async commands, and a `std` guard held across an `.await` deadlocks.

## Background updating

A loop ticks every 60 seconds, reads the due list, fetches, then writes. The
first tick waits four seconds after launch so it does not compete with the
window's first paint, and is skipped when "Update all feeds when SnapRSS
starts" is off — that switch previously controlled nothing. The
three steps are separate because the lock must not be held across the network:
the obvious version hands `&mut Db` to the fetcher and awaits with it held, so
every click in the app blocks behind a poll nobody asked for, once a minute.
That is what made SnapRSS feel like it stalled at random.

`fetch_all` touches no database; `apply_fetched` does no network. `update_due`
still does both in one call for tests and one-shot tools.

Progress is emitted per feed as `update-progress` — by the background loop as
well as by the button — so the toolbar shows a bar and the status line names
the feed being fetched.

The Update button fetches **every** feed, not the ones the schedule says are
due. `all_feeds` answers "what did the user ask for", `due_feeds` answers "what
should the loop poll now". Using the schedule for a button press meant clicking
it usually did nothing and reported "nothing due yet", which looked like broken
feedback rather than a no-op. Conditional GET keeps it cheap: an unchanged feed
is a 304.

## Opening an article

`article` never waits on the network. It returns the cached extraction if there
is one, otherwise the feed's own summary, and starts the fetch in the
background; `article-ready` arrives when the real article has been extracted
and cached, and the frontend swaps it in if that article is still open.

Extraction runs in `spawn_blocking` — readability is CPU-bound and would
otherwise sit on an async runtime thread. One extraction runs per article at a
time, so clicking through a list does not start a fetch per click.

## Frontend

`ui/index.html` plus `ui/app.js`, hand written. No npm, no bundler, no build
step; `frontendDist` points straight at the directory.

Two things are load-bearing:

* `app.withGlobalTauri: true` in `tauri.conf.json`. Without it
  `window.__TAURI__` does not exist and nothing in `app.js` runs.
* The script lives in its own file. The CSP sets `script-src 'self'`, so an
  inline `<script>` is blocked and the frontend does not run.

`window.prompt` is not implemented in WebView2 and `window.confirm` is
unreliable across Tauri's platforms. Both are replaced by the `ask()` modal in
`app.js`.

### Testing the frontend

    cd crates/app/ui-test && npm install && npm test

Runs the frontend in jsdom against a stubbed Tauri bridge, clicks every button,
and checks the right command was invoked with the right arguments. It also
asserts the two invariants above statically: a harness that evaluates `app.js`
by hand cannot notice a CSP that would have blocked it.

If a framework is added later, point `frontendDist` at its output directory and
add `beforeDevCommand` and `beforeBuildCommand` to `tauri.conf.json`.

All eight themes are `:root[data-theme=...]` blocks of CSS variables. Both
densities are a `data-density` attribute that changes row height, font sizes
and toolbar heights.

Keyboard: `J` / `K` next and previous, `Shift+J` / `Shift+K` extend the
selection, `S` star, `M` toggle read, `L` labels, `Shift+M` mark all read,
`B` or `Enter` open in browser, `Delete` delete, `Ctrl+A` select all, `Escape`
clear the selection, `/` search, `F5` update.

### Dialogs

`ask()`, the label editor and the filter editor refuse to open while another
dialog is up, and sit at z-index 80, above the Settings sheet (55) and context
menus (60). A dialog under the sheet was invisible, so every further click on
Delete opened another behind it. Focus moves into the dialog, so Enter cannot
re-press the button behind it either.

### Nothing is selected at launch

The list starts empty and says "Select a feed." It stays empty through the
start-up poll and any later update until a feed, folder or category is
clicked. Removing the feed being viewed goes back to that state rather than
jumping somewhere else.

### Focus and selection are two different things

`state.selected` is the article the reading pane is showing. `state.sel` is a
Set of ids that toolbar actions apply to. A plain click sets both; ctrl-click
changes only the selection; shift-click takes a range from the anchor.

Either can point at a row that is gone: a read article leaves the Unread scope
on the next refresh, a deleted one leaves every scope. `state.items.find(...)`
may return nothing, so callers go through `focusedItem()`, `targetIds()` and
`itemsFor()`. Reaching into the result directly produced "cannot read
properties of undefined (reading 'read')". `loadList()` prunes ids the new list
no longer contains.

Bulk actions follow the mail-client rule: if any of the selection is off, turn
them all on.

### Undo

`Ctrl+Z` reverses the last delete, restore or mark-all-read. Only reversible
things go on the stack: deleting an article is a soft delete and comes straight
back, while unsubscribing a feed is a cascade, so that asks first rather than
implying undo will save you.

`Delete` means the selected feed when the tree has focus and the selected
articles otherwise. It used to always mean the articles, so pressing Delete
over the tree silently deleted whatever was open in the reading pane.

## Running it headlessly

No desktop needed. On a machine with the Linux dependencies installed:

    Xvfb :99 -screen 0 1440x900x24 -ac &
    DISPLAY=:99 WEBKIT_DISABLE_COMPOSITING_MODE=1 GDK_BACKEND=x11 \
      ./target/debug/snaprss-app &
    DISPLAY=:99 import -window root shot.png      # ImageMagick
    DISPLAY=:99 xdotool mousemove 400 140 click 1 # drive it

`cargo run -p snaprss-fetch --example seed -- <db> <file.opml>` fills a database
from an OPML file and fetches everything once.

## The sidebar

Feeds on top, Categories below, as QuiteRSS arranges them.

Folders fold. The chevron toggles, the rest of the row opens the folder, and
Left / Right do the same from the keyboard. The state lives in `feeds.expanded`,
the same column QuiteRSS uses, so it survives a restart and an import keeps
whatever was collapsed there. Hovering over a collapsed folder while dragging
opens it after a moment, so a feed can be dropped beside one of its children.

Feeds and folders drag. Two things are required for that to work at all in
WebView2, and neither shows up on Linux:

* `dragDropEnabled: false` on the window. Tauri intercepts drag and drop at the
  OS level by default, which stops HTML5 drag and drop inside the page on
  Windows. Its own schema says so.
* Tree rows are `<div role="button">`, not `<button>`. Chromium will not start
  a drag from a form control however `draggable` is set. WebKitGTK will, which
  is exactly why this passed here and failed there.

A row splits into thirds: the edges mean "beside", the
middle of a folder means "inside". A feed has no inside, so its row splits in
half. Empty space under the tree is the root, which is how a feed leaves a
folder. The categories panel has its own scroll and collapses; its state is in
localStorage.

## Settings

`Ctrl+,` or the app menu. Nine pages: General, Appearance, Toolbars, Reading,
Labels, Filters, Storage & cleanup, Shortcuts, About.

Most pages edit a pending set and commit on Save. Toolbars, labels and filters
write through immediately, like theme and density: their effect is visible
behind the window.

Global options are key/value rows in `settings`, not a typed struct: the set
changes often and the frontend is the only consumer, so typing them would mean
a migration per checkbox. Per-feed overrides live in the feed's properties
window.

Theme, density, layout and the toolbar arrangement are per-machine, so they
stay in localStorage and are not written to the database at all.

### The four toolbars

Main, Feeds, News and Reading, as QuiteRSS has them. Each can be hidden, shown
as icons, icons and text, or text only, and its buttons reordered, removed or
added back with separators anywhere.

Any command can go on any bar. The one real element for each lives on its home
bar; `seedToolbarClones` copies it into the others at startup and forwards the
clone's click to the original, so there is still exactly one handler per
command.

Customisation moves the existing buttons and toggles `hidden`; it never rebuilds
them. Rebuilding each bar from a registry drops every handler bound at startup,
so a test clicks a button after moving it.

Two rules keep the app usable after customisation. The app menu cannot be
removed from the main bar, and if it ends up with nowhere to live the main bar
is switched back on — otherwise hiding every toolbar locks Settings away. And
text-only mode hides an icon only where there is a label to replace it, or a
button with no label of its own becomes invisible.

A saved arrangement is filtered against the current build and de-duplicated on
load, so a command dropped in a later version disappears instead of breaking
the bar. The fallback is a copy of the default list: handing out the module's
own array let the first edit mutate the defaults, and Reset then restored the
corrupted version.

### Panels

The three pane widths and the categories height are custom properties on
`:root`, so a drag is one property write and the grid re-lays itself. The grips
are 4px of grid track with a wider invisible grab area, take pointer capture so
the drag survives the pointer outrunning them, respond to arrow keys, and reset
on double-click. Sizes are in localStorage.

### Layouts

Classic is the three-pane reader. Newspaper is one wide column of cards that
expand in place; it hides the reading pane rather than narrowing it.

The excerpt under each headline is built in Rust and arrives as plain text. It
comes from feed HTML, and the frontend never receives markup that did not go
through the sanitiser.

### Labels

Created and coloured on the Labels page; applied from the list's Labels button,
the context menu, or `L`. The menu ticks a label when every selected article
carries it, so clicking a ticked one removes it from all of them. Filters can
apply labels as articles arrive.

Deleting a label leaves the articles alone.

## The app menu

The hamburger in the top left. Submenus open on hover and stack; the stack
closes on Escape or a click elsewhere. Grouped as QuiteRSS groups it: Add,
Import/Export/Backup, View, Feeds, News, Tools, Help, Exit.

## Startup and the tray

`tauri-plugin-autostart` writes the login item with no arguments; whether the
window starts minimised is the `startup.minimized` setting alone. An enabled
login item is rewritten at startup, which clears the `--minimized` argument
earlier builds put there and which forced a minimised start. The login item is
OS state, not a database setting, so the switch writes through immediately and
is stripped before the rest of the settings are saved.

The window's place, size and maximised state are Windows' own record of them,
`GetWindowPlacement`, stored as `window.placement` and put back with
`SetWindowPlacement` before the first paint. That record keeps the restored
rectangle while the window is maximised or minimised, and says whether a
minimised window will come back maximised. The window-state plugin tracked
move events instead, and saved the (-8, -8) a maximised window sits at as its
position; and a window minimised to the tray read as not maximised.

A second launch hands over to the running copy (single-instance plugin) and
brings its window forward, rather than starting a second tray icon and a
second poller on the same database.

`startup.close_to_tray` is mirrored into an `AtomicBool` on `AppState`, because
the window's close handler is synchronous and cannot wait on the database lock.
Minimise-to-tray is handled in the frontend instead: the window event fires
before the platform minimises, and hiding from there is the only way to get the
taskbar button to go with it.

Start-minimised hides rather than minimises when there is a tray icon to get
the window back from, and minimises when there is not.

On Linux the tray needs a session bus; without one the icon is skipped and the
app runs normally.

## Colours and the CSP

Tauri rewrites the CSP in `tauri.conf.json` and puts a nonce on `style-src`. A
nonce makes `'unsafe-inline'` inert for style attributes as well as style
elements, so `style="background:…"` written into `innerHTML` is dropped by the
webview. jsdom does not enforce CSP, so it cannot see this.

Runtime colours travel on `data-bg` / `data-fg` and are applied by `paint()`
through the CSSOM, which CSP does not govern. Fixed colours belong in the
stylesheet. A static check in the harness fails if a colour is authored into a
style attribute.

`tint()` writes `hsl(h, s%, l%)` with commas for a similar reason: older
WebKitGTK drops the space-separated form.

## Security notes

Article HTML is sanitised in Rust by `ammonia` against an allowlist before it
reaches the webview, which is why the frontend can assign it with `innerHTML`.
The CSP in `tauri.conf.json` blocks inline and remote script regardless, so a
sanitiser bug is not immediately an execution bug.

Links inside articles are intercepted and handed to the system browser through
`tauri-plugin-opener`. Nothing navigates the webview itself.

`capabilities/default.json` grants file dialogs, clipboard writing, URL
opening, and hiding and showing the window for the tray. Tauri's default window
permissions are read-only getters; anything that changes the window has to be
granted by name, and a missing one only shows up in the real app as "Command
plugin:window|… not allowed by ACL". The harness checks every
`getCurrentWindow().x()` call in `app.js` against the granted list. The opener entry is an object, not a bare string:
`opener:allow-open-url` authorises the command, but without an `allow` scope
listing permitted URLs the plugin rejects every one with "Not allowed to open
url".

The CSP must keep `http:` in `img-src`. Feeds serve images over plain http, and
without it most articles render with blank boxes.

## Icons

On Windows, Tauri builds the window icon from the **first** entry of
`icon.ico` and sets it as the small icon only. The large icon — the one the
taskbar reads — and the window-class icon are left empty, so the taskbar fell
back to the generic application icon while the title bar looked fine.
`set_window_icons` in `main.rs` loads resource 32512 (where tauri-build embeds
`icon.ico`) at the window's DPI and sends both `WM_SETICON` messages. The Windows
code is type-checked with `cargo check --target x86_64-pc-windows-gnu`.

The artwork is a white RSS glyph on dark denim (#072142). `ui/logo.png` is
the same art at 128 px for the About page, so the two cannot drift.

`icon.ico` carries 16, 20, 24, 32, 40, 48, 64, 128 and 256 px: the sizes the
title bar and taskbar ask for at 100–200% scaling. Up to 64 px they are
uncompressed 32-bit bitmaps and only 128 and 256 are PNG, which is the layout
Windows handles everywhere. The 32 px image is first because that is the one
Tauri picks.

`icons/` holds a generated set: `icon.ico` (16 through 256, PNG-compressed
entries), `icon.png` at 512, the three sizes Tauri's bundler expects, and the
Windows Store square logos. `icon.ico` is required on Windows: `tauri-build`
generates a Windows Resource file from it and fails the build without it.

To use your own artwork, drop a square PNG of at least 1024x1024 in and run:

    cd crates/app && cargo tauri icon path/to/your.png

That regenerates every file in `icons/`, including the macOS `.icns` the
current set lacks. `bundle.icon` already points at the right names.
