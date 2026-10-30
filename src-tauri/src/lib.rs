mod actions;
mod background;
mod database;
mod downloads;
mod engine;
mod models;
mod removable;

use models::{CapabilityStatus, EngineResult, PreviewOperation, RunRecord, WorkerHealth, Workflow, WorkflowRuntimeStatus};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    AppHandle, Manager, State, WindowEvent,
};
use tauri_plugin_notification::NotificationExt;

struct AppState {
    db_path: PathBuf,
    health: Arc<Mutex<WorkerHealth>>,
    shutdown: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
    execution_lock: Arc<Mutex<()>>,
}

#[tauri::command]
fn execute_workflow(
    workflow: Workflow,
    dry_run: bool,
    state: State<AppState>,
    app: AppHandle,
) -> Result<EngineResult, String> {
    let _guard = state.execution_lock.lock().map_err(|_| "Execution lock is unavailable")?;
    state.cancelled.store(false, Ordering::Relaxed);
    database::sync_workflow(&state.db_path, &workflow)?;
    let result = engine::execute(&workflow, dry_run, &state.db_path, "manual", None, false, &state.cancelled, None)?;
    if !dry_run {
        send_notifications(&app, &result);
    }
    Ok(result)
}

#[tauri::command]
fn stop_current_run(state: State<AppState>) {
    state.cancelled.store(true, Ordering::Relaxed);
}

#[tauri::command]
fn sync_workflows(
    workflows: Vec<Workflow>,
    state: State<AppState>,
) -> Result<Vec<WorkflowRuntimeStatus>, String> {
    for workflow in &workflows {
        engine::validate_graph(workflow)?;
        database::sync_workflow(&state.db_path, workflow)?;
    }
    database::statuses(&state.db_path)
}

#[tauri::command]
fn set_workflow_enabled(
    workflow: Workflow,
    enabled: bool,
    state: State<AppState>,
) -> Result<WorkflowRuntimeStatus, String> {
    engine::validate_graph(&workflow)?;
    database::set_enabled(&state.db_path, &workflow, enabled)
}

#[tauri::command]
fn workflow_statuses(state: State<AppState>) -> Result<Vec<WorkflowRuntimeStatus>, String> {
    database::statuses(&state.db_path)
}

#[tauri::command]
fn worker_health(state: State<AppState>) -> Result<WorkerHealth, String> {
    state
        .health
        .lock()
        .map(|health| health.clone())
        .map_err(|_| "Background engine status is unavailable".into())
}

#[tauri::command]
fn list_run_history(state: State<AppState>) -> Result<Vec<RunRecord>, String> {
    database::list_runs(&state.db_path)
}

#[tauri::command]
fn capability_statuses() -> Vec<CapabilityStatus> {
    actions::capability_statuses()
}

#[tauri::command]
fn store_secret(name: String, secret: String) -> Result<(), String> {
    if name.trim().is_empty() || secret.is_empty() {
        return Err("Secret name and value are required".into());
    }
    actions::set_secret(name.trim(), &secret)
}

#[tauri::command]
fn remove_secret(name: String) -> Result<(), String> {
    actions::delete_secret(name.trim())
}

#[tauri::command]
fn undo_run(run_id: String, state: State<AppState>) -> Result<usize, String> {
    let _guard = state.execution_lock.lock().map_err(|_| "Execution lock is unavailable")?;
    actions::undo_run(&state.db_path, &run_id)
}

pub(crate) fn send_notifications(app: &AppHandle, result: &EngineResult) {
    if let Some((title, body)) = organizer_notification(result) {
        let _ = app.notification().builder().title(title).body(body).show();
        return;
    }
    for operation in result.operations.iter().filter(|operation| operation.action == "notification") {
        let body = operation.destination.as_deref().unwrap_or("Workflow complete");
        let _ = app.notification().builder().title(&result.run.workflow_name).body(body).show();
    }
}

fn organizer_notification(result: &EngineResult) -> Option<(String, String)> {
    if !result.operations.iter().any(|operation| operation.action == "notification") {
        return None;
    }
    let organized: Vec<&PreviewOperation> = result
        .operations
        .iter()
        .filter(|operation| {
            matches!(
                operation.action.as_str(),
                "organize_downloads" | "extract_rom_archive" | "extract_package_archive"
            )
        })
        .collect();
    if organized.is_empty() {
        return None;
    }

    let mut lines: Vec<String> = organized.iter().take(4).filter_map(|operation| {
        let source = Path::new(operation.source.as_deref()?).file_name()?.to_string_lossy();
        let destination = compact_path(operation.destination.as_deref()?);
        let suffix = matches!(operation.action.as_str(), "extract_rom_archive" | "extract_package_archive")
            .then_some(" (extracted)")
            .unwrap_or_default();
        Some(format!("{} → {}{}", truncate(&source, 52), truncate(&destination, 76), suffix))
    }).collect();
    if organized.len() > lines.len() {
        lines.push(format!("+{} more", organized.len() - lines.len()));
    }
    let count = organized.len();
    Some((
        format!("Organized {count} download{}", if count == 1 { "" } else { "s" }),
        lines.join("\n"),
    ))
}

fn compact_path(raw: &str) -> String {
    let path = Path::new(raw);
    dirs::home_dir()
        .and_then(|home| path.strip_prefix(home).ok().map(|relative| format!("~/{}", relative.display())))
        .unwrap_or_else(|| raw.to_owned())
}

fn truncate(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_owned();
    }
    let mut shortened: String = value.chars().take(limit.saturating_sub(1)).collect();
    shortened.push('…');
    shortened
}

#[cfg(test)]
mod notification_tests {
    use super::*;
    use std::collections::HashMap;

    fn operation(action: &str, source: Option<&str>, destination: Option<&str>) -> PreviewOperation {
        PreviewOperation {
            node_id: action.into(),
            node_label: action.into(),
            action: action.into(),
            source: source.map(str::to_owned),
            destination: destination.map(str::to_owned),
            warning: None,
        }
    }

    #[test]
    fn download_notification_names_the_file_and_destination() {
        let home = dirs::home_dir().unwrap();
        let result = EngineResult {
            run: RunRecord {
                id: "run".into(),
                workflow_id: "downloads".into(),
                workflow_name: "Downloads".into(),
                started_at: "now".into(),
                duration_ms: 1,
                status: "success".into(),
                files: 1,
                reversible: true,
                message: String::new(),
            },
            operations: vec![
                operation(
                    "organize_downloads",
                    Some("/tmp/game.pkg"),
                    Some(&home.join("Programs/PS4/game.pkg").display().to_string()),
                ),
                operation("notification", None, Some("New downloads were organized")),
            ],
            node_statuses: HashMap::new(),
        };
        let (title, body) = organizer_notification(&result).unwrap();
        assert_eq!(title, "Organized 1 download");
        assert_eq!(body, "game.pkg → ~/Programs/PS4/game.pkg");
    }

    #[test]
    fn extracted_rom_notification_says_it_was_extracted() {
        let result = EngineResult {
            run: RunRecord {
                id: "run".into(), workflow_id: "downloads".into(), workflow_name: "Downloads".into(),
                started_at: "now".into(), duration_ms: 1, status: "success".into(), files: 1,
                reversible: false, message: String::new(),
            },
            operations: vec![
                operation("extract_rom_archive", Some("/tmp/Mario.zip"), Some("/tmp/Roms/N64")),
                operation("notification", None, Some("done")),
            ],
            node_statuses: HashMap::new(),
        };
        let (_, body) = organizer_notification(&result).unwrap();
        assert_eq!(body, "Mario.zip → /tmp/Roms/N64 (extracted)");
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let start_hidden = std::env::args_os().any(|argument| argument == "--background");
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, arguments, _working_directory| {
            if !arguments.iter().any(|argument| argument == "--background") {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
        }))
        .plugin(tauri_plugin_notification::init())
        .setup(move |app| {
            let data_dir = app.path().app_data_dir()?;
            fs::create_dir_all(&data_dir)?;
            let db_path = data_dir.join("conduit.sqlite3");
            database::initialize(&db_path).map_err(std::io::Error::other)?;
            let ssd_workflow = removable::ps4usb_workflow();
            if !database::workflow_exists(&db_path, &ssd_workflow.id).map_err(std::io::Error::other)? {
                database::approve_preview(&db_path, &ssd_workflow).map_err(std::io::Error::other)?;
                database::set_enabled(&db_path, &ssd_workflow, true).map_err(std::io::Error::other)?;
            }

            let health = Arc::new(Mutex::new(WorkerHealth::default()));
            let shutdown = Arc::new(AtomicBool::new(false));
            let cancelled = Arc::new(AtomicBool::new(false));
            let execution_lock = Arc::new(Mutex::new(()));
            background::spawn(db_path.clone(), health.clone(), shutdown.clone(), cancelled.clone(), execution_lock.clone(), Some(app.handle().clone()));
            app.manage(AppState { db_path, health, shutdown, cancelled, execution_lock });

            let show = MenuItem::with_id(app, "show", "Show Conduit", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit Conduit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;
            let mut tray = TrayIconBuilder::new().menu(&menu).tooltip("Conduit background automation");
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.on_menu_event(|app, event| match event.id.as_ref() {
                "show" => {
                    if let Some(window) = app.get_webview_window("main") {
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                }
                "quit" => {
                    app.state::<AppState>().shutdown.store(true, Ordering::Relaxed);
                    app.exit(0);
                }
                _ => {}
            })
            .build(app)?;
            if !start_hidden {
                if let Some(window) = app.get_webview_window("main") {
                    window.show()?;
                    window.set_focus()?;
                }
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            execute_workflow,
            sync_workflows,
            set_workflow_enabled,
            workflow_statuses,
            worker_health,
            stop_current_run,
            list_run_history
            ,capability_statuses
            ,store_secret
            ,remove_secret
            ,undo_run
        ])
        .run(tauri::generate_context!())
        .expect("error while running Conduit");
}
