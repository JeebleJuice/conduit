mod actions;
mod background;
mod database;
mod downloads;
mod engine;
mod models;

use models::{CapabilityStatus, EngineResult, RunRecord, WorkerHealth, Workflow, WorkflowRuntimeStatus};
use std::{
    fs,
    path::PathBuf,
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
    for operation in result.operations.iter().filter(|operation| operation.action == "notification") {
        let body = operation.destination.as_deref().unwrap_or("Workflow complete");
        let _ = app.notification().builder().title(&result.run.workflow_name).body(body).show();
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            fs::create_dir_all(&data_dir)?;
            let db_path = data_dir.join("conduit.sqlite3");
            database::initialize(&db_path).map_err(std::io::Error::other)?;

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
