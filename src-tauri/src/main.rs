#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use session_loom_core::{
    canonical::{CanonicalSession, SourceTool},
    daemon::{self, DaemonState},
    delete::{self, DeleteResult},
    paths,
    restore::{restore_session, RestoreResult, RestoreRoots},
    store::{ListFilter, SessionCard, Store},
    trash::{restore_from_trash, Trash, TrashEntry},
};
use std::{
    path::{Path, PathBuf},
    process::Command,
    str::FromStr,
};
use tauri::{
    menu::MenuBuilder,
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, Runtime, WindowEvent,
};

const MAIN_WINDOW_LABEL: &str = "main";
const TRAY_OPEN_ID: &str = "open";
const TRAY_QUIT_ID: &str = "quit";

#[derive(Debug, PartialEq, Eq)]
enum TrayMenuAction {
    ShowWindow,
    Quit,
    Ignore,
}

fn should_hide_on_close(window_label: &str) -> bool {
    window_label == MAIN_WINDOW_LABEL
}

fn tray_menu_action(menu_id: &str) -> TrayMenuAction {
    match menu_id {
        TRAY_OPEN_ID => TrayMenuAction::ShowWindow,
        TRAY_QUIT_ID => TrayMenuAction::Quit,
        _ => TrayMenuAction::Ignore,
    }
}

fn show_main_window<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
        let _ = window.emit("window-shown", ());
    }
}

fn install_tray<R: Runtime>(app: &tauri::App<R>) -> tauri::Result<()> {
    let menu = MenuBuilder::new(app)
        .text(TRAY_OPEN_ID, "打开 Session-Loom")
        .separator()
        .text(TRAY_QUIT_ID, "退出 Session-Loom")
        .build()?;
    let mut tray = TrayIconBuilder::new()
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip("Session-Loom 正在后台运行")
        .on_menu_event(|app, event| match tray_menu_action(event.id().as_ref()) {
            TrayMenuAction::ShowWindow => show_main_window(app),
            TrayMenuAction::Quit => app.exit(0),
            TrayMenuAction::Ignore => {}
        })
        .on_tray_icon_event(|tray, event| {
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            ) {
                show_main_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    #[cfg(target_os = "macos")]
    {
        tray = tray.icon_as_template(true);
    }
    tray.build(app)?;
    Ok(())
}

fn store() -> Result<Store, String> {
    Store::from_environment()
}

#[tauri::command]
async fn sessions_list(filter: Option<ListFilter>) -> Result<Vec<SessionCard>, String> {
    tauri::async_runtime::spawn_blocking(move || store()?.list_cards(filter.unwrap_or_default()))
        .await
        .map_err(|error| format!("session list task failed: {error}"))?
}

#[tauri::command]
async fn sessions_get(session_id: String) -> Result<CanonicalSession, String> {
    tauri::async_runtime::spawn_blocking(move || store()?.read_session(&session_id))
        .await
        .map_err(|error| format!("session read task failed: {error}"))?
}

#[tauri::command]
fn sessions_delete(session_id: String) -> DeleteResult {
    let store = match store() {
        Ok(store) => store,
        Err(message) => {
            return DeleteResult {
                ok: false,
                message,
                source_deleted: false,
            }
        }
    };
    delete::delete_session(&store, &session_id, &RestoreRoots::from_environment())
}

#[tauri::command]
async fn trash_list() -> Result<Vec<TrashEntry>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let store = store()?;
        Trash::new(store.root()).list()
    })
    .await
    .map_err(|error| format!("trash list task failed: {error}"))?
}

#[tauri::command]
async fn trash_restore(session_id: String) -> Result<CanonicalSession, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let store = store()?;
        let trash = Trash::new(store.root());
        restore_from_trash(&store, &trash, &session_id)
    })
    .await
    .map_err(|error| format!("trash restore task failed: {error}"))?
}

#[tauri::command]
fn trash_delete(session_id: String) -> Result<(), String> {
    let store = store()?;
    Trash::new(store.root()).remove(&session_id)
}

#[tauri::command]
async fn sessions_restore(session_id: String, target: String) -> RestoreResult {
    tauri::async_runtime::spawn_blocking(move || {
        let target = match SourceTool::from_str(&target) {
            Ok(target) => target,
            Err(message) => return RestoreResult { ok: false, message },
        };
        let store = match store() {
            Ok(store) => store,
            Err(message) => return RestoreResult { ok: false, message },
        };
        restore_session(
            &store,
            target,
            Some(&session_id),
            &RestoreRoots::from_environment(),
        )
    })
    .await
    .unwrap_or_else(|error| RestoreResult {
        ok: false,
        message: format!("session restore task failed: {error}"),
    })
}

fn cli_binary_name() -> &'static str {
    if cfg!(windows) {
        "ssl.exe"
    } else {
        "ssl"
    }
}

fn cli_executable() -> Result<PathBuf, String> {
    if let Ok(path) = std::env::var("SESSION_LOOM_CLI") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        return Err("SESSION_LOOM_CLI does not point to a file".to_string());
    }
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let directory = executable
        .parent()
        .ok_or_else(|| "desktop executable has no parent directory".to_string())?;
    let name = cli_binary_name();
    let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    first_existing_cli(cli_candidates(directory, name, &project_root))
        .ok_or_else(|| "Rust ssl CLI executable was not found".to_string())
}

fn cli_candidates(directory: &Path, name: &str, project_root: &Path) -> Vec<PathBuf> {
    vec![
        directory.join(name),
        directory.join("bin").join(name),
        directory.join("resources").join("bin").join(name),
        directory
            .join("..")
            .join("Resources")
            .join("bin")
            .join(name),
        project_root.join("target").join("debug").join(name),
        project_root.join("target").join("release").join(name),
    ]
}

fn first_existing_cli<I>(candidates: I) -> Option<PathBuf>
where
    I: IntoIterator<Item = PathBuf>,
{
    candidates.into_iter().find(|candidate| candidate.is_file())
}

fn ensure_daemon_running() -> DaemonState {
    match cli_executable() {
        Ok(executable) => daemon::ensure_daemon_running(&paths::store_root(), &executable),
        Err(_) => DaemonState {
            running: false,
            pid: None,
        },
    }
}

#[tauri::command]
fn daemon_status() -> DaemonState {
    daemon::daemon_state(&paths::store_root())
}

#[tauri::command]
fn open_github() -> Result<(), String> {
    const URL: &str = "https://github.com/JC0v0/session-loom";
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("cmd");
        command.args(["/C", "start", "", URL]);
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("open");
        command.arg(URL);
        command
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = {
        let mut command = Command::new("xdg-open");
        command.arg(URL);
        command
    };
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    command
        .status()
        .map_err(|error| error.to_string())
        .and_then(|status| {
            status
                .success()
                .then_some(())
                .ok_or_else(|| "无法打开 GitHub 仓库".to_string())
        })
}

#[tauri::command]
async fn daemon_toggle() -> DaemonState {
    tauri::async_runtime::spawn_blocking(|| {
        let store_root = paths::store_root();
        let state = daemon::daemon_state(&store_root);
        if state.running {
            daemon::stop_daemon(&store_root)
        } else {
            ensure_daemon_running()
        }
    })
    .await
    .unwrap_or(DaemonState {
        running: false,
        pid: None,
    })
}

fn main() {
    let app = tauri::Builder::default()
        .setup(|app| {
            install_tray(app)?;
            let _ = ensure_daemon_running();
            Ok(())
        })
        .on_window_event(|window, event| {
            if !should_hide_on_close(window.label()) {
                return;
            }
            let WindowEvent::CloseRequested { api, .. } = event else {
                return;
            };
            api.prevent_close();
            let _ = window.hide();
            // Closing hides the window to the tray instead of quitting; tell
            // the frontend so it can reset transient views (e.g. the recycle
            // bin) before the next open.
            let _ = window.emit("window-hidden", ());
        })
        .invoke_handler(tauri::generate_handler![
            sessions_list,
            sessions_get,
            sessions_delete,
            sessions_restore,
            trash_list,
            trash_restore,
            trash_delete,
            daemon_status,
            daemon_toggle,
            open_github
        ])
        .build(tauri::generate_context!())
        .expect("error while building session-loom");
    app.run(|app, event| {
        let _ = (app, event);
        #[cfg(target_os = "macos")]
        if let tauri::RunEvent::Reopen {
            has_visible_windows: false,
            ..
        } = event
        {
            show_main_window(app);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{
        cli_candidates, first_existing_cli, should_hide_on_close, tray_menu_action, TrayMenuAction,
    };
    use std::fs;

    #[test]
    fn hides_only_the_main_window_when_close_is_requested() {
        assert!(should_hide_on_close("main"));
        assert!(!should_hide_on_close("settings"));
    }

    #[test]
    fn maps_tray_menu_items_to_explicit_actions() {
        assert_eq!(tray_menu_action("open"), TrayMenuAction::ShowWindow);
        assert_eq!(tray_menu_action("quit"), TrayMenuAction::Quit);
        assert_eq!(tray_menu_action("unknown"), TrayMenuAction::Ignore);
    }

    #[test]
    fn selects_the_first_existing_rust_cli() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("missing");
        let existing = directory.path().join("ssl");
        fs::write(&existing, "binary").unwrap();
        assert_eq!(
            first_existing_cli([missing, existing.clone()]),
            Some(existing)
        );
    }

    #[test]
    fn finds_the_cli_in_the_bundled_resource_directory() {
        let directory = tempfile::tempdir().unwrap();
        let executable_directory = directory.path().join("app");
        let bundled_cli = executable_directory.join("bin").join("ssl");
        fs::create_dir_all(bundled_cli.parent().unwrap()).unwrap();
        fs::write(&bundled_cli, "binary").unwrap();

        assert_eq!(
            first_existing_cli(cli_candidates(
                &executable_directory,
                "ssl",
                &directory.path().join("project")
            )),
            Some(bundled_cli)
        );
    }
}
