//! What filters ask for beyond changing the article: a sound and a
//! notification. Both happen here, after an update is written, because the
//! filter engine and the fetcher have no way to play or show anything.

use tauri::{AppHandle, Emitter};

use crate::commands::{CommandError, Res};

/// Audio formats a filter may point at. Also the limit on what `read_sound`
/// will hand to the page, so it cannot be used to read other files.
const AUDIO: &[&str] = &["wav", "mp3", "ogg", "oga", "opus", "m4a", "aac", "flac", "wma"];
const MAX_SOUND_BYTES: u64 = 10 * 1024 * 1024;

fn is_audio(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| AUDIO.contains(&e.to_ascii_lowercase().as_str()))
}

/// Sounds and notifications for one finished update.
pub fn announce(app: &AppHandle, summary: &snaprss_fetch::UpdateSummary) {
    for path in &summary.sounds {
        play(app, path);
    }
    notify(app, &summary.notices);
}

/// One notification per update, however many articles matched: a burst of
/// twenty toasts after a long sleep would be worse than none.
fn notify(app: &AppHandle, notices: &[(String, String)]) {
    use tauri_plugin_notification::NotificationExt;
    let (title, body) = match notices {
        [] => return,
        [(feed, article)] => (feed.clone(), article.clone()),
        many => {
            let mut body: Vec<String> = many.iter().take(4).map(|(_, a)| format!("• {a}")).collect();
            if many.len() > 4 {
                body.push(format!("and {} more", many.len() - 4));
            }
            (format!("{} new articles matched your filters", many.len()), body.join("\n"))
        }
    };
    if let Err(e) = app.notification().builder().title(title).body(body).show() {
        tracing::warn!(error = %e, "notification");
    }
}

/// Play a sound file. On Windows a .wav goes straight to the system's
/// PlaySound, which works with the window hidden in the tray; anything else
/// is played by the page.
pub fn play(app: &AppHandle, path: &str) {
    let p = std::path::Path::new(path.trim());
    if !is_audio(p) {
        tracing::warn!(path, "not a sound file");
        return;
    }
    #[cfg(windows)]
    if p.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("wav")) {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_FILENAME, SND_NODEFAULT};
        let wide: Vec<u16> = p.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        // SAFETY: a NUL-terminated wide path that outlives the call; with
        // SND_ASYNC PlaySound copies what it needs before returning.
        let ok = unsafe { PlaySoundW(wide.as_ptr(), std::ptr::null_mut(), SND_FILENAME | SND_ASYNC | SND_NODEFAULT) };
        if ok != 0 {
            return;
        }
    }
    let _ = app.emit("play-sound", path);
}

/// The bytes of a sound file, for the page to play. Only audio files, and
/// only up to 10 MB.
#[tauri::command]
pub async fn read_sound(path: String) -> Res<tauri::ipc::Response> {
    let p = std::path::PathBuf::from(path.trim());
    if !is_audio(&p) {
        return Err(CommandError::Invalid("not a sound file".into()));
    }
    let meta = std::fs::metadata(&p).map_err(|e| CommandError::Invalid(format!("{}: {e}", p.display())))?;
    if meta.len() > MAX_SOUND_BYTES {
        return Err(CommandError::Invalid("that sound file is over 10 MB".into()));
    }
    let bytes = std::fs::read(&p).map_err(|e| CommandError::Invalid(format!("{}: {e}", p.display())))?;
    Ok(tauri::ipc::Response::new(bytes))
}

/// The filter editor's Play button.
#[tauri::command]
pub async fn test_sound(app: AppHandle, path: String) -> Res<()> {
    let p = std::path::Path::new(path.trim());
    if !is_audio(p) {
        return Err(CommandError::Invalid("choose a sound file (.wav, .mp3, .ogg …)".into()));
    }
    if !p.exists() {
        return Err(CommandError::Invalid(format!("{} does not exist", p.display())));
    }
    play(&app, &path);
    Ok(())
}
