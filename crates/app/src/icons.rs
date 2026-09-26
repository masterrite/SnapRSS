//! Site icons for the feed list.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, Emitter, Manager, State};

use crate::commands::{AppState, Res};
use snaprss_core::DbError;

/// Only one lookup runs at a time; a slow one is not joined by the next tick's.
static RUNNING: AtomicBool = AtomicBool::new(false);

/// Every feed's icon, as a `data:` URL, by feed id.
#[tauri::command]
pub async fn feed_icons(state: State<'_, AppState>) -> Res<HashMap<i64, String>> {
    let db = state.db.lock().await;
    Ok(db.feed_icons()?.into_iter().collect())
}

/// Look up icons for feeds that have none, a few at a time, in the
/// background. The database lock is held only to read the list and to
/// store what came back, never across the network.
pub fn refresh_in_background(app: &AppHandle, limit: usize) {
    if RUNNING.swap(true, Ordering::AcqRel) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let _ = refresh(&app, None, limit).await;
        RUNNING.store(false, Ordering::Release);
    });
}

/// Look up the icon of one feed now: the one just added.
#[tauri::command]
pub async fn refresh_feed_icon(app: AppHandle, id: i64) -> Res<bool> {
    Ok(refresh(&app, Some(id), 1).await?)
}

async fn refresh(app: &AppHandle, only: Option<i64>, limit: usize) -> Result<bool, DbError> {
    let state = app.state::<AppState>();
    let todo: Vec<(i64, String)> = {
        let db = state.db.lock().await;
        match only {
            Some(id) => db
                .conn()
                .query_row(
                    "SELECT id, COALESCE(NULLIF(html_url, ''), xml_url) FROM feeds
                     WHERE id = ?1 AND kind = 1 AND (image IS NULL OR length(image) = 0)",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map(|row| vec![row])
                .unwrap_or_default(),
            None => db.feeds_needing_icons(limit)?,
        }
    };
    if todo.is_empty() {
        return Ok(false);
    }
    let mut set = tokio::task::JoinSet::new();
    for (id, site) in todo {
        let client = state.http.clone();
        let cfg = state.fetch_cfg.clone();
        set.spawn(async move {
            let icon = snaprss_fetch::icons::find_icon(&client, &cfg, &site).await;
            (id, site, icon)
        });
    }
    let mut found = Vec::new();
    while let Some(Ok(r)) = set.join_next().await {
        found.push(r);
    }
    let any = found.iter().any(|(_, _, b)| b.is_some());
    {
        let db = state.db.lock().await;
        for (id, site, bytes) in &found {
            db.store_icon(*id, site, bytes.as_deref())?;
        }
    }
    if any {
        let _ = app.emit("icons-updated", ());
    }
    Ok(any)
}
