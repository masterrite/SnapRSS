// Stops a console window appearing behind the app on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod alerts;
mod commands;
mod icons;
mod tray_badge;
#[cfg(test)]
mod bench;

use std::sync::Arc;

use snaprss_core::Db;
use snaprss_fetch::{build_client, FetchConfig};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::Manager;
use tauri_plugin_autostart::MacosLauncher;
use tokio::sync::Mutex;

use commands::AppState;

/// How often the background loop looks for feeds that are due. Individual feeds
/// have their own intervals; this is just the tick.
const TICK_SECONDS: u64 = 60;
const DEFAULT_INTERVAL_MINUTES: i64 = 15;
/// Retention runs on a slow cadence of its own. It only ever marks things
/// deleted, never removes them, so it is safe to run unattended.
const CLEANUP_EVERY_TICKS: u64 = 60;
/// How long after launch the first automatic poll waits, so it does not
/// compete with the window's first paint.
const STARTUP_POLL_DELAY_SECONDS: u64 = 4;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "snaprss=info,warn".into()),
        )
        .init();

    tauri::Builder::default()
        // First, so a second launch hands over before anything else starts:
        // started again from the Start menu while it sat in the tray, it ran
        // a second copy, with a second tray icon and a second poller writing
        // to the same database.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| reveal(app)))
        .plugin(tauri_plugin_opener::init())
        // Endpoints are set per check from the `updates.repo` setting; the
        // public key the downloads are verified against is in tauri.conf.json.
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        // The startup entry passes no arguments: whether the window starts
        // minimised is the "Start minimised" setting and nothing else.
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, None))
        .setup(|app| {
            // One SQLite file in the platform's app-data directory:
            // %APPDATA%\SnapRSS on Windows, ~/.local/share/SnapRSS on Linux,
            // ~/Library/Application Support/SnapRSS on macOS.
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            let path = dir.join("snaprss.db");
            tracing::info!(?path, "opening database");

            let db = Db::open(&path)?;

            // Settings that have to be known before the window is shown or the
            // first poll runs. Read from this connection rather than opening
            // the file a second time.
            let setting = |k: &str, default: bool| -> bool {
                db.conn()
                    .query_row("SELECT value FROM settings WHERE key = ?1", [k], |r| {
                        r.get::<_, String>(0)
                    })
                    .map(|v| v == "1")
                    .unwrap_or(default)
            };
            let start_minimised = setting("startup.minimized", false);
            let to_tray = setting("startup.close_to_tray", false);
            let update_on_start = setting("update.on_startup", true);
            let min_to_tray = setting("startup.minimize_to_tray", false);
            let placement: Option<String> = db
                .conn()
                .query_row(
                    "SELECT value FROM settings WHERE key = 'window.placement'",
                    [],
                    |r| r.get(0),
                )
                .ok();

            let cfg = FetchConfig::default();
            let http = build_client(&cfg)?;

            let close_to_tray = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let quitting = Arc::new(std::sync::atomic::AtomicBool::new(false));

            app.manage(AppState {
                db: Arc::new(Mutex::new(db)),
                http: http.clone(),
                fetch_cfg: cfg.clone(),
                close_to_tray: close_to_tray.clone(),
                quitting,
                pending_update: Default::default(),
            });

            setup_tray(app.handle())?;
            {
                let h = app.handle().clone();
                tauri::async_runtime::spawn(async move { tray_badge::refresh(&h).await });
            }

            close_to_tray.store(to_tray, std::sync::atomic::Ordering::Relaxed);

            // Earlier builds wrote the startup entry with `--minimized`, which
            // started the window minimised regardless of the setting.
            // Rewriting it drops the argument.
            {
                use tauri_plugin_autostart::ManagerExt;
                let mgr = app.autolaunch();
                if mgr.is_enabled().unwrap_or(false) {
                    let _ = mgr.enable();
                }
            }

            if let Some(w) = app.get_webview_window("main") {
                #[cfg(windows)]
                set_window_icons(&w);
                // The window is created hidden (tauri.conf.json), moved and
                // sized here while still hidden, and shown once the page has
                // drawn its content (`window_ready`), so it opens in its
                // place at its size.
                //
                // Starting minimised hides it when the user keeps SnapRSS in
                // the tray either way; minimising to the taskbar would
                // contradict that.
                let start = if !start_minimised {
                    Start::Normal
                } else if to_tray || min_to_tray {
                    Start::Hidden
                } else {
                    Start::Minimised
                };
                #[cfg(windows)]
                let placed = placement.as_deref().and_then(|p| placement::place_hidden(&w, p, start));
                #[cfg(not(windows))]
                let placed: Option<bool> = {
                    let _ = placement;
                    None
                };
                if placed.is_none() {
                    first_run_size(&w);
                }
                let max = placed.unwrap_or(false);
                if start == Start::Hidden {
                    MAXIMISE_ON_REVEAL.store(max, std::sync::atomic::Ordering::Relaxed);
                } else {
                    *PENDING_SHOW.lock().unwrap() = Some((start, max));
                    // If the page never says it is ready, show it anyway.
                    let h = app.handle().clone();
                    tauri::async_runtime::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_secs(4)).await;
                        show_pending(&h);
                    });
                }
            }

            // Background updater. Feeds are only fetched when their own
            // interval says so; this loop just wakes up and asks.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                // The first tick of a tokio interval fires at once, which put a
                // full poll of every feed on top of the window's first paint.
                // Give the window a few seconds, and skip the start-up poll
                // entirely when the user has switched it off.
                let first = std::time::Duration::from_secs(if update_on_start {
                    STARTUP_POLL_DELAY_SECONDS
                } else {
                    TICK_SECONDS
                });
                let mut ticker = tokio::time::interval_at(
                    tokio::time::Instant::now() + first,
                    std::time::Duration::from_secs(TICK_SECONDS),
                );
                // After sleep or hibernation, one tick, not one for every
                // minute missed: the default fires them all back to back.
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                // With "update on startup" off, feeds wait one full interval
                // from launch. Only delaying the first tick still polled every
                // stale feed a minute after starting.
                let poll_from = if update_on_start {
                    tokio::time::Instant::now()
                } else {
                    let minutes = {
                        let state = handle.state::<AppState>();
                        let db = state.db.lock().await;
                        global_interval_minutes(&db)
                    };
                    tokio::time::Instant::now()
                        + std::time::Duration::from_secs(minutes.max(1) as u64 * 60)
                };
                let mut ticks: u64 = 0;
                loop {
                    ticker.tick().await;
                    ticks += 1;

                    #[cfg(windows)]
                    if let Some(w) = handle.get_webview_window("main") {
                        placement::save(&w).await;
                    }

                    // New version: a few minutes after start, then daily.
                    if ticks == UPDATE_CHECK_FIRST_TICK || ticks.is_multiple_of(UPDATE_CHECK_EVERY_TICKS) {
                        let auto = {
                            let state = handle.state::<AppState>();
                            let db = state.db.lock().await;
                            db.conn()
                                .query_row(
                                    "SELECT value FROM settings WHERE key = 'updates.auto'",
                                    [],
                                    |r| r.get::<_, String>(0),
                                )
                                .map(|v| v != "0")
                                .unwrap_or(true)
                        };
                        if auto {
                            match commands::find_update(&handle).await {
                                Ok(Some(info)) => {
                                    use tauri::Emitter;
                                    let _ = handle.emit("update-available", &info);
                                }
                                Ok(None) => {}
                                // Offline, or no release published yet: nothing to say.
                                Err(e) => tracing::info!(error = %e, "update check"),
                            }
                        }
                    }

                    // Retention, hourly. Deliberately only the reversible half:
                    // an automatic run never removes a row, so a rule set by
                    // mistake costs nothing but a trip to the Deleted folder.
                    if ticks.is_multiple_of(CLEANUP_EVERY_TICKS) {
                        let state = handle.state::<AppState>();
                        let mut db = state.db.lock().await;
                        match snaprss_core::cleanup::run(&mut db, None) {
                            Ok(r) if !r.is_empty() => {
                                tracing::info!(
                                    by_read = r.by_read,
                                    by_age = r.by_age,
                                    by_count = r.by_count,
                                    "cleanup"
                                );
                                drop(db);
                                use tauri::Emitter;
                                let _ = handle.emit("feeds-updated", 0);
                            }
                            Ok(_) => {}
                            Err(e) => tracing::warn!(error = %e, "cleanup failed"),
                        }
                    }

                    if tokio::time::Instant::now() < poll_from {
                        continue;
                    }

                    // Icons for feeds without one, a dozen a minute, alongside.
                    icons::refresh_in_background(&handle, 12);

                    // Fetch with the lock released. Holding it across the
                    // network is what made the whole app stall for as long as
                    // the slowest feed took to answer, once a minute.
                    let state = handle.state::<AppState>();
                    let due = {
                        let db = state.db.lock().await;
                        let global = global_interval_minutes(&db);
                        match snaprss_fetch::due_feeds(&db, chrono::Utc::now(), global) {
                            Ok(f) => f,
                            Err(e) => {
                                tracing::warn!(error = %e, "due_feeds failed");
                                continue;
                            }
                        }
                    };
                    if due.is_empty() {
                        continue;
                    }

                    // The background poll reports progress too, so "the app is
                    // fetching" is visible whenever it happens and not only
                    // when the button was pressed.
                    let h = handle.clone();
                    let fetched = snaprss_fetch::fetch_all(
                        &state.http,
                        &state.fetch_cfg,
                        due,
                        6,
                        move |p| {
                            use tauri::Emitter;
                            let _ = h.emit(
                                "update-progress",
                                serde_json::json!({
                                    "done": p.done, "total": p.total,
                                    "title": p.title, "ok": p.ok,
                                }),
                            );
                        },
                    )
                    .await;

                    let mut db = state.db.lock().await;
                    match snaprss_fetch::apply_fetched(&mut db, fetched) {
                        Ok(s) if s.attempted > 0 => {
                            tracing::info!(
                                attempted = s.attempted,
                                new = s.new_articles,
                                not_modified = s.not_modified,
                                failed = s.failed,
                                "update tick"
                            );
                            drop(db);
                            alerts::announce(&handle, &s);
                            tray_badge::refresh(&handle).await;
                            use tauri::Emitter;
                            let _ = handle.emit("feeds-updated", s.new_articles);
                        }
                        Ok(_) => {}
                        Err(e) => tracing::warn!(error = %e, "update tick failed"),
                    }
                }
            });

            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                #[cfg(windows)]
                if let Some(w) = window.app_handle().get_webview_window("main") {
                    placement::save_now(&w);
                }
                // Close means hide when the user asked for that. The tray menu
                // and Exit set `quitting` first, so there is always a way out.
                let state = window.state::<AppState>();
                if state.close_to_tray.load(std::sync::atomic::Ordering::Relaxed)
                    && !state.quitting.load(std::sync::atomic::Ordering::Relaxed)
                {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::feed_tree,
            commands::news_list,
            commands::article,
            commands::counts,
            commands::set_read,
            commands::set_starred,
            commands::set_deleted,
            commands::mark_scope_read,
            commands::add_feed,
            commands::remove_feed,
            commands::rename_node,
            commands::add_folder,
            commands::set_reading_mode,
            commands::update_all,
            commands::update_feed_now,
            commands::discover_feed,
            window_ready,
            icons::feed_icons,
            icons::refresh_feed_icon,
            alerts::read_sound,
            alerts::test_sound,
            commands::import_file,
            commands::export_opml,
            commands::get_settings,
            commands::set_settings,
            commands::db_stats,
            commands::run_cleanup,
            commands::purge_deleted,
            commands::vacuum_db,
            commands::clear_article_cache,
            commands::backup_db,
            commands::feed_settings,
            commands::save_feed_settings,
            commands::apply_settings_to_folder,
            commands::labels,
            commands::save_label,
            commands::delete_label,
            commands::set_label,
            commands::clear_labels,
            commands::reorder_label,
            commands::filters,
            commands::filter_vocabulary,
            commands::save_filter,
            commands::delete_filter,
            commands::set_filter_enabled,
            commands::reorder_filter,
            commands::apply_filters_now,
            commands::move_node,
            commands::set_expanded,
            commands::set_autostart,
            commands::autostart_enabled,
            commands::set_close_to_tray,
            commands::quit_app,
            commands::check_update,
            commands::install_update,
        ])
        .run(tauri::generate_context!())
        .expect("failed to start SnapRSS");
}

/// Tray icon with a small menu. Left-clicking it toggles the window, which is
/// what every tray app on Windows does.
fn setup_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Show SnapRSS", true, None::<&str>)?;
    let update = MenuItem::with_id(app, "update", "Update all feeds", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &update, &quit])?;

    TrayIconBuilder::with_id("main")
        .icon(app.default_window_icon().cloned().unwrap())
        .tooltip("SnapRSS")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => reveal(app),
            "update" => {
                use tauri::Emitter;
                let _ = app.emit("tray-update", ());
                reveal(app);
            }
            "quit" => {
                #[cfg(windows)]
                if let Some(w) = app.get_webview_window("main") {
                    placement::save_now(&w);
                }
                let state = app.state::<AppState>();
                state
                    .quitting
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let app = tray.app_handle();
                if let Some(w) = app.get_webview_window("main") {
                    // Not "and focused": clicking the notification area takes
                    // focus from the window before this event arrives, so the
                    // window never counted as focused and never hid.
                    if w.is_visible().unwrap_or(false) && !w.is_minimized().unwrap_or(false) {
                        #[cfg(windows)]
                        placement::save_now(&w);
                        let _ = w.hide();
                    } else {
                        reveal(app);
                    }
                }
            }
        })
        .build(app)?;
    Ok(())
}

const UPDATE_CHECK_FIRST_TICK: u64 = 3;
const UPDATE_CHECK_EVERY_TICKS: u64 = 24 * 60;

/// The "Update every" setting. Read on every tick so a change applies without
/// a restart.
fn global_interval_minutes(db: &Db) -> i64 {
    db.conn()
        .query_row(
            "SELECT value FROM settings WHERE key = 'update.interval_minutes'",
            [],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|m| *m > 0)
        .unwrap_or(DEFAULT_INTERVAL_MINUTES)
}

/// Where the window was and whether it was maximised, as Windows itself
/// records it.
///
/// `GetWindowPlacement` keeps the restored ("normal") rectangle while the
/// window is maximised or minimised, and says whether a minimised window will
/// come back maximised. The window-state plugin tracked move events instead:
/// maximising moves the window to (-8, -8), which it saved as the position,
/// and a window minimised to the tray reported "not maximised" when saved.
#[cfg(windows)]
pub mod placement {
    use std::sync::Mutex;
    use tauri::Manager;
    use windows_sys::Win32::Foundation::{HWND, POINT, RECT};
    use windows_sys::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromRect, MONITORINFO, MONITOR_DEFAULTTONEAREST};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowPlacement, SetWindowPlacement, SetWindowPos, ShowWindow, SWP_NOACTIVATE, SWP_NOZORDER,
        SW_HIDE, SW_SHOWMAXIMIZED, SW_SHOWMINIMIZED, SW_SHOWMINNOACTIVE, SW_SHOWNORMAL, WINDOWPLACEMENT,
        WPF_RESTORETOMAXIMIZED,
    };

    use crate::commands::AppState;
    use crate::Start;

    /// Last value written, so a quiet minute costs nothing.
    static LAST: Mutex<String> = Mutex::new(String::new());

    fn hwnd(w: &tauri::WebviewWindow) -> Option<HWND> {
        w.hwnd().ok().map(|h| h.0 as HWND)
    }

    /// "left,top,right,bottom,maximised"
    pub fn read(w: &tauri::WebviewWindow) -> Option<String> {
        let hwnd = hwnd(w)?;
        unsafe {
            let mut wp: WINDOWPLACEMENT = std::mem::zeroed();
            wp.length = std::mem::size_of::<WINDOWPLACEMENT>() as u32;
            if GetWindowPlacement(hwnd, &mut wp) == 0 {
                return None;
            }
            let r = wp.rcNormalPosition;
            if r.right - r.left < 200 || r.bottom - r.top < 150 {
                return None;
            }
            let max = wp.showCmd == SW_SHOWMAXIMIZED as u32
                || (wp.showCmd == SW_SHOWMINIMIZED as u32 && wp.flags & WPF_RESTORETOMAXIMIZED != 0);
            Some(format!("{},{},{},{},{}", r.left, r.top, r.right, r.bottom, max as u8))
        }
    }

    /// The saved rectangle, kept to put back as the restore size after the
    /// window is shown maximised.
    static NORMAL: Mutex<Option<RECT>> = Mutex::new(None);

    fn placement_of(rect: RECT, show: u32, flags: u32) -> WINDOWPLACEMENT {
        // SAFETY: WINDOWPLACEMENT is plain data; zero is a valid value.
        let mut wp: WINDOWPLACEMENT = unsafe { std::mem::zeroed() };
        wp.length = std::mem::size_of::<WINDOWPLACEMENT>() as u32;
        wp.showCmd = show;
        wp.flags = flags;
        wp.ptMinPosition = POINT { x: -1, y: -1 };
        wp.ptMaxPosition = POINT { x: -1, y: -1 };
        wp.rcNormalPosition = rect;
        wp
    }

    /// Move and size the still-hidden window to where it was. Returns
    /// whether it was maximised, or None when there was nothing usable to
    /// restore.
    ///
    /// Everything that changes the window's size happens here, before it is
    /// first shown: the web view inside follows the window's size a moment
    /// late, and moving to a monitor with a different scaling makes Windows
    /// resize the window again. Done after showing, both were visible as the
    /// window opening at the default size and then jumping.
    pub fn place_hidden(w: &tauri::WebviewWindow, saved: &str, start: Start) -> Option<bool> {
        let v: Vec<i32> = saved.split(',').filter_map(|p| p.trim().parse().ok()).collect();
        let [left, top, right, bottom, max] = v[..] else { return None };
        if right - left < 200 || bottom - top < 150 {
            return None;
        }
        let hwnd = hwnd(w)?;
        let rect = RECT { left, top, right, bottom };
        let max = max != 0;
        // SAFETY: a live window handle, and placements built above.
        unsafe {
            // Twice: the first move can land on a monitor with other scaling,
            // which resizes the window; the second puts the exact size back.
            // Windows brings a rectangle that is off every monitor (one since
            // unplugged) back onto the screen by itself.
            SetWindowPlacement(hwnd, &placement_of(rect, SW_HIDE as u32, 0));
            SetWindowPlacement(hwnd, &placement_of(rect, SW_HIDE as u32, 0));
            if max && start == Start::Normal {
                // Laid out at the maximised size too, so showing it maximised
                // does not resize the page either. The restore size is put
                // back once it is shown.
                let mon = MonitorFromRect(&rect, MONITOR_DEFAULTTONEAREST);
                let mut mi: MONITORINFO = std::mem::zeroed();
                mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
                if GetMonitorInfoW(mon, &mut mi) != 0 {
                    let a = mi.rcWork;
                    SetWindowPos(hwnd, std::ptr::null_mut(), a.left, a.top, a.right - a.left, a.bottom - a.top,
                                 SWP_NOACTIVATE | SWP_NOZORDER);
                }
            }
        }
        *NORMAL.lock().unwrap() = Some(rect);
        *LAST.lock().unwrap() = saved.to_string();
        Some(max)
    }

    /// Show the window placed by [`place_hidden`].
    pub fn show(w: &tauri::WebviewWindow, start: Start, max: bool) {
        let Some(hwnd) = hwnd(w) else { return };
        let normal = *NORMAL.lock().unwrap();
        // SAFETY: a live window handle.
        unsafe {
            match (start, normal) {
                (Start::Normal, Some(rect)) if max => {
                    ShowWindow(hwnd, SW_SHOWMAXIMIZED);
                    // Already maximised, so this only sets where Restore goes.
                    SetWindowPlacement(hwnd, &placement_of(rect, SW_SHOWMAXIMIZED as u32, 0));
                }
                (Start::Normal, _) => {
                    ShowWindow(hwnd, if max { SW_SHOWMAXIMIZED } else { SW_SHOWNORMAL });
                }
                (Start::Minimised, Some(rect)) => {
                    let flags = if max { WPF_RESTORETOMAXIMIZED } else { 0 };
                    SetWindowPlacement(hwnd, &placement_of(rect, SW_SHOWMINNOACTIVE as u32, flags));
                }
                (Start::Minimised, None) => {
                    ShowWindow(hwnd, SW_SHOWMINNOACTIVE);
                }
                (Start::Hidden, _) => {}
            }
        }
    }

    fn changed(w: &tauri::WebviewWindow) -> Option<String> {
        let now = read(w)?;
        let mut last = LAST.lock().unwrap();
        if *last == now {
            return None;
        }
        last.clone_from(&now);
        Some(now)
    }

    const SQL: &str = "INSERT INTO settings(key, value) VALUES('window.placement', ?1)
                       ON CONFLICT(key) DO UPDATE SET value = excluded.value";

    /// From the background loop, once a minute.
    pub async fn save(w: &tauri::WebviewWindow) {
        if !crate::SHOWN_ONCE.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        if let Some(v) = changed(w) {
            let db = w.state::<AppState>().db.clone();
            let db = db.lock().await;
            let _ = db.conn().execute(SQL, [v]);
        }
    }

    /// From a window event or just before quitting. Blocks until written:
    /// handed to the runtime instead, the write lost the race with the
    /// process exiting.
    pub fn save_now(w: &tauri::WebviewWindow) {
        tauri::async_runtime::block_on(save(w));
    }
}

/// How the window starts.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Start {
    Normal,
    Minimised,
    /// In the tray, not on the taskbar.
    Hidden,
}

/// Set when the window started hidden in the tray and was maximised when
/// SnapRSS last closed: a hidden window cannot carry "maximised", so the first
/// time it is shown it is maximised then.
static MAXIMISE_ON_REVEAL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether the window has been on screen since SnapRSS started. Until it
/// has, its placement is not saved: a window started in the tray is not
/// maximised yet, and saving it would forget that it should be.
pub static SHOWN_ONCE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// How the window is to be shown once the page is ready.
static PENDING_SHOW: std::sync::Mutex<Option<(Start, bool)>> = std::sync::Mutex::new(None);

/// Show the window placed at startup, once.
fn show_pending(app: &tauri::AppHandle) {
    let Some((start, max)) = PENDING_SHOW.lock().unwrap().take() else { return };
    let Some(w) = app.get_webview_window("main") else { return };
    let w2 = w.clone();
    SHOWN_ONCE.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = w.run_on_main_thread(move || {
        #[cfg(windows)]
        {
            placement::show(&w2, start, max);
            // Tao keeps its own "visible" flag and only changes it when it
            // shows the window itself. Shown behind its back, the window
            // counted as hidden, and every hide() after that did nothing:
            // close-to-tray and the tray icon's click stopped hiding it.
            // SW_SHOW here keeps the size and state just set.
            let _ = w2.show();
        }
        #[cfg(not(windows))]
        match start {
            Start::Normal => {
                let _ = w2.show();
                if max {
                    let _ = w2.maximize();
                }
            }
            Start::Minimised => {
                let _ = w2.minimize();
            }
            Start::Hidden => {}
        }
        if start == Start::Normal {
            let _ = w2.set_focus();
        }
    });
}

/// The page has drawn its content: show the window.
#[tauri::command]
fn window_ready(app: tauri::AppHandle) {
    show_pending(&app);
}

/// No saved place yet: most of the screen it is on, centred, rather than a
/// fixed size that is too big for a laptop and small on a large monitor.
fn first_run_size(w: &tauri::WebviewWindow) {
    let monitor = w.current_monitor().ok().flatten().or_else(|| w.primary_monitor().ok().flatten());
    let Some(m) = monitor else { return };
    let area = m.work_area();
    let scale = m.scale_factor();
    let min_w = (900.0 * scale) as u32;
    let min_h = (560.0 * scale) as u32;
    let width = ((area.size.width as f64 * 0.8) as u32).max(min_w).min(area.size.width);
    let height = ((area.size.height as f64 * 0.85) as u32).max(min_h).min(area.size.height);
    let x = area.position.x + (area.size.width.saturating_sub(width) / 2) as i32;
    let y = area.position.y + (area.size.height.saturating_sub(height) / 2) as i32;
    let _ = w.set_size(tauri::PhysicalSize::new(width, height));
    let _ = w.set_position(tauri::PhysicalPosition::new(x, y));
}

fn reveal(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        SHOWN_ONCE.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = w.show();
        let _ = w.unminimize();
        if MAXIMISE_ON_REVEAL.swap(false, std::sync::atomic::Ordering::Relaxed) {
            let _ = w.maximize();
        }
        let _ = w.set_focus();
    }
}

/// Give the window proper small and large icons from the icon resource that
/// tauri-build embeds in the executable.
///
/// Tauri only ever sets the small icon (`ICON_SMALL`), from the first entry of
/// icon.ico, and leaves the large one and the window class icon empty. The
/// title bar uses the small icon and looked fine; the taskbar asks for the
/// large one, found nothing, and showed the generic Windows application icon.
/// Loading both from the resource at the window's DPI lets Windows pick the
/// right image for each size instead of scaling one.
#[cfg(windows)]
fn set_window_icons(window: &tauri::WebviewWindow) {
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        LoadImageW, SendMessageW, ICON_BIG, ICON_SMALL, IMAGE_ICON, LR_DEFAULTCOLOR, SM_CXICON,
        SM_CXSMICON, SM_CYICON, SM_CYSMICON, WM_SETICON,
    };

    /// The resource ID tauri-build embeds icon.ico under.
    const ICON_RESOURCE: usize = 32512;

    let Ok(hwnd) = window.hwnd() else { return };
    let hwnd = hwnd.0 as HWND;

    unsafe {
        let module = GetModuleHandleW(std::ptr::null());
        if module.is_null() {
            return;
        }
        let dpi = match GetDpiForWindow(hwnd) {
            0 => 96,
            d => d,
        };
        let load = |cx: i32, cy: i32| {
            LoadImageW(
                module,
                ICON_RESOURCE as *const u16,
                IMAGE_ICON,
                cx,
                cy,
                LR_DEFAULTCOLOR,
            )
        };

        let big = load(
            GetSystemMetricsForDpi(SM_CXICON, dpi),
            GetSystemMetricsForDpi(SM_CYICON, dpi),
        );
        if !big.is_null() {
            SendMessageW(hwnd, WM_SETICON, ICON_BIG as usize, big as isize);
        }
        let small = load(
            GetSystemMetricsForDpi(SM_CXSMICON, dpi),
            GetSystemMetricsForDpi(SM_CYSMICON, dpi),
        );
        if !small.is_null() {
            SendMessageW(hwnd, WM_SETICON, ICON_SMALL as usize, small as isize);
        }
    }
}
