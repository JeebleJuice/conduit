mod background;
mod database;
mod engine;
mod models;

use models::{EngineResult, RunRecord, WorkerHealth, Workflow, WorkflowRuntimeStatus};
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
    Manager, State, WindowEvent,
};

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
) -> Result<EngineResult, String> {
    let _guard = state.execution_lock.lock().map_err(|_| "Execution lock is unavailable")?;
    state.cancelled.store(false, Ordering::Relaxed);
    database::sync_workflow(&state.db_path, &workflow)?;
    engine::execute(&workflow, dry_run, &state.db_path, "manual", None, false, &state.cancelled)
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            fs::create_dir_all(&data_dir)?;
            let db_path = data_dir.join("conduit.sqlite3");
            database::initialize(&db_path).map_err(std::io::Error::other)?;

            let health = Arc::new(Mutex::new(WorkerHealth::default()));
            let shutdown = Arc::new(AtomicBool::new(false));
            let cancelled = Arc::new(AtomicBool::new(false));
            let execution_lock = Arc::new(Mutex::new(()));
            background::spawn(db_path.clone(), health.clone(), shutdown.clone(), cancelled.clone(), execution_lock.clone());
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running Conduit");
}
