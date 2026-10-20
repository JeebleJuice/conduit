use crate::{
    database,
    engine::{self, expand_home, number_config, string_config, trigger_node},
    models::{WorkerHealth, Workflow},
};
use chrono::{Datelike, Local, Timelike, Utc};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tauri::AppHandle;
use std::{
    collections::{HashMap, HashSet},
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

pub(crate) fn spawn(
    db_path: PathBuf,
    health: Arc<Mutex<WorkerHealth>>,
    shutdown: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
    execution_lock: Arc<Mutex<()>>,
    app: Option<AppHandle>,
) {
    thread::Builder::new()
        .name("conduit-background-engine".into())
        .spawn(move || run_loop(&db_path, &health, &shutdown, &cancelled, &execution_lock, &app))
        .expect("failed to start the Conduit background engine");
}

fn run_loop(
    db_path: &Path,
    health: &Arc<Mutex<WorkerHealth>>,
    shutdown: &Arc<AtomicBool>,
    cancelled: &Arc<AtomicBool>,
    execution_lock: &Arc<Mutex<()>>,
    app: &Option<AppHandle>,
) {
    if let Ok(mut value) = health.lock() {
        value.running = true;
    }
    let (event_tx, event_rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |event| {
        let _ = event_tx.send(event);
    })
    .ok();
    let mut watched = HashSet::new();
    let mut workflows = Vec::new();
    let mut startup_fired = HashSet::new();
    let mut schedule_fired: HashMap<String, String> = HashMap::new();
    let mut fingerprints: HashMap<String, u64> = HashMap::new();
    let mut last_refresh = Instant::now() - Duration::from_secs(30);
    let mut last_reconcile = Instant::now();
    let mut cooldowns: HashMap<String, Instant> = HashMap::new();
    let mut webhooks: HashMap<String, TcpListener> = HashMap::new();
    let mut mounted = mount_points();
    let mut last_mount_check = Instant::now();

    while !shutdown.load(Ordering::Relaxed) {
        if last_refresh.elapsed() >= Duration::from_secs(5) {
            match database::load_enabled(db_path) {
                Ok(current) => {
                    workflows = current;
                    refresh_watches(&workflows, watcher.as_mut(), &mut watched);
                    refresh_webhooks(&workflows, &mut webhooks);
                    if let Ok(mut value) = health.lock() {
                        value.enabled_workflows = workflows.len();
                        value.watched_folders = watched.len();
                        value.last_error = None;
                    }
                    for workflow in &workflows {
                        if trigger_node(workflow).is_some_and(|node| node.data.node_type == "startup")
                            && startup_fired.insert(workflow.id.clone())
                        {
                            run_with_retries(db_path, workflow, "startup", None, cancelled, execution_lock, app, None);
                        }
                    }
                }
                Err(error) => set_error(health, error),
            }
            last_refresh = Instant::now();
        }

        for workflow in &workflows {
            if schedule_is_due(workflow, &mut schedule_fired) {
                run_with_retries(db_path, workflow, "schedule", None, cancelled, execution_lock, app, None);
            }
        }

        for workflow in &workflows {
            let Some(listener) = webhooks.get(&workflow.id) else { continue };
            if let Ok((mut stream, _)) = listener.accept() {
                let mut request = [0_u8; 8192];
                let size = stream.read(&mut request).unwrap_or_default();
                let request = String::from_utf8_lossy(&request[..size]);
                let wanted_path = trigger_node(workflow)
                    .and_then(|node| string_config(node, "path"))
                    .unwrap_or_else(|| "/conduit".into());
                let valid = request.lines().next().is_some_and(|line| {
                    let mut parts = line.split_whitespace();
                    matches!(parts.next(), Some("POST")) && parts.next() == Some(wanted_path.as_str())
                });
                let response = if valid {
                    "HTTP/1.1 202 Accepted\r\nContent-Length: 8\r\nConnection: close\r\n\r\naccepted"
                } else {
                    "HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\nConnection: close\r\n\r\nnot found"
                };
                let _ = stream.write_all(response.as_bytes());
                if valid && !recently_ran(&mut cooldowns, &workflow.id) {
                    let body = request.split("\r\n\r\n").nth(1).unwrap_or_default();
                    run_with_retries(db_path, workflow, "local webhook", None, cancelled, execution_lock, app, Some(body));
                }
            }
        }

        if last_mount_check.elapsed() >= Duration::from_secs(2) {
            let current = mount_points();
            for path in current.difference(&mounted) {
                for workflow in &workflows {
                    let Some(trigger) = trigger_node(workflow).filter(|node| node.data.node_type == "drive_mounted") else { continue };
                    let matches_name = string_config(trigger, "mountName").is_none_or(|wanted| {
                        path.file_name().is_some_and(|name| name.to_string_lossy().to_lowercase().contains(&wanted.to_lowercase()))
                    });
                    if matches_name && !recently_ran(&mut cooldowns, &workflow.id) {
                        let mounted_path = path.display().to_string();
                        run_with_retries(db_path, workflow, "drive mounted", None, cancelled, execution_lock, app, Some(&mounted_path));
                    }
                }
            }
            mounted = current;
            last_mount_check = Instant::now();
        }

        match event_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(Ok(event)) => {
                for workflow in &workflows {
                    let Some(folder) = watched_folder(workflow) else { continue };
                    let matching: Vec<PathBuf> = event
                        .paths
                        .iter()
                        .filter(|path| path.starts_with(&folder))
                        .cloned()
                        .collect();
                    if matching.is_empty() || recently_ran(&mut cooldowns, &workflow.id) {
                        continue;
                    }
                    run_with_retries(db_path, workflow, "folder change", Some(&matching), cancelled, execution_lock, app, None);
                }
            }
            Ok(Err(error)) => set_error(health, format!("Filesystem watcher: {error}")),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                set_error(health, "Filesystem watcher stopped unexpectedly".into());
                watcher = None;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }

        if last_reconcile.elapsed() >= Duration::from_secs(60) {
            for workflow in &workflows {
                let Some(folder) = watched_folder(workflow) else { continue };
                let current = folder_fingerprint(&folder);
                if let Some(previous) = fingerprints.insert(workflow.id.clone(), current) {
                    if previous != current && !recently_ran(&mut cooldowns, &workflow.id) {
                        run_with_retries(db_path, workflow, "reconciliation", None, cancelled, execution_lock, app, None);
                    }
                }
            }
            if let Ok(mut value) = health.lock() {
                value.last_reconcile_at = Some(Utc::now().to_rfc3339());
            }
            last_reconcile = Instant::now();
        }
    }
    if let Ok(mut value) = health.lock() {
        value.running = false;
    }
}

fn refresh_webhooks(workflows: &[Workflow], listeners: &mut HashMap<String, TcpListener>) {
    let desired: HashSet<String> = workflows
        .iter()
        .filter(|workflow| trigger_node(workflow).is_some_and(|node| node.data.node_type == "webhook"))
        .map(|workflow| workflow.id.clone())
        .collect();
    listeners.retain(|id, _| desired.contains(id));
    for workflow in workflows {
        if listeners.contains_key(&workflow.id) {
            continue;
        }
        let Some(trigger) = trigger_node(workflow).filter(|node| node.data.node_type == "webhook") else { continue };
        let port = number_config(trigger, "port").unwrap_or(43_821.0).round() as u16;
        if let Ok(listener) = TcpListener::bind(("127.0.0.1", port)) {
            let _ = listener.set_nonblocking(true);
            listeners.insert(workflow.id.clone(), listener);
        }
    }
}

fn mount_points() -> HashSet<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        let user = std::env::var("USER").unwrap_or_default();
        [PathBuf::from("/media"), PathBuf::from("/run/media").join(user)]
            .into_iter()
            .filter_map(|root| std::fs::read_dir(root).ok())
            .flatten()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect()
    }
    #[cfg(target_os = "macos")]
    {
        std::fs::read_dir("/Volumes")
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect()
    }
    #[cfg(target_os = "windows")]
    {
        (b'D'..=b'Z')
            .map(|letter| PathBuf::from(format!("{}:\\", letter as char)))
            .filter(|path| path.exists())
            .collect()
    }
}

fn refresh_watches(
    workflows: &[Workflow],
    watcher: Option<&mut RecommendedWatcher>,
    watched: &mut HashSet<PathBuf>,
) {
    let desired: HashSet<PathBuf> = workflows.iter().filter_map(watched_folder).collect();
    let Some(watcher) = watcher else { return };
    for removed in watched.difference(&desired) {
        let _ = watcher.unwatch(removed);
    }
    for added in desired.difference(watched) {
        if added.is_dir() {
            let _ = watcher.watch(added, RecursiveMode::NonRecursive);
        }
    }
    *watched = desired;
}

fn watched_folder(workflow: &Workflow) -> Option<PathBuf> {
    trigger_node(workflow)
        .filter(|node| node.data.node_type == "folder_changed")
        .and_then(|node| string_config(node, "path"))
        .and_then(|path| expand_home(&path).ok())
        .and_then(|path| path.canonicalize().ok())
}

fn recently_ran(cooldowns: &mut HashMap<String, Instant>, workflow_id: &str) -> bool {
    let now = Instant::now();
    let recent = cooldowns
        .get(workflow_id)
        .is_some_and(|previous| now.duration_since(*previous) < Duration::from_secs(2));
    if !recent {
        cooldowns.insert(workflow_id.to_owned(), now);
    }
    recent
}

fn schedule_is_due(workflow: &Workflow, fired: &mut HashMap<String, String>) -> bool {
    let Some(trigger) = trigger_node(workflow).filter(|node| node.data.node_type == "schedule") else {
        return false;
    };
    let now = Local::now();
    let minute_key = now.format("%Y-%m-%dT%H:%M").to_string();
    if fired.get(&workflow.id) == Some(&minute_key) {
        return false;
    }
    let due = if let Some(interval) = number_config(trigger, "intervalMinutes") {
        let minute = now.timestamp().div_euclid(60);
        interval >= 1.0 && minute % interval.round() as i64 == 0
    } else {
        string_config(trigger, "schedule")
            .is_some_and(|schedule| cron_matches(&schedule, &now))
    };
    if due {
        fired.insert(workflow.id.clone(), minute_key);
    }
    due
}

fn cron_matches(expression: &str, now: &chrono::DateTime<Local>) -> bool {
    let fields: Vec<&str> = expression.split_whitespace().collect();
    if fields.len() != 5 {
        return false;
    }
    field_matches(fields[0], now.minute())
        && field_matches(fields[1], now.hour())
        && field_matches(fields[2], now.day())
        && field_matches(fields[3], now.month())
        && field_matches(fields[4], now.weekday().num_days_from_sunday())
}

fn field_matches(field: &str, value: u32) -> bool {
    field == "*"
        || field
            .split(',')
            .filter_map(|part| part.parse::<u32>().ok())
            .any(|candidate| candidate == value)
}

fn folder_fingerprint(folder: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(folder) else { return 0 };
    entries.filter_map(Result::ok).fold(0_u64, |fingerprint, entry| {
        let Ok(metadata) = entry.metadata() else { return fingerprint };
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs())
            .unwrap_or_default();
        fingerprint.wrapping_add(metadata.len()).rotate_left(7) ^ modified
    })
}

fn retry_count(workflow: &Workflow) -> usize {
    workflow
        .nodes
        .iter()
        .filter_map(|node| node.data.config.get("retryCount").and_then(serde_json::Value::as_u64))
        .max()
        .unwrap_or(1)
        .clamp(1, 3) as usize
}

fn run_with_retries(
    db_path: &Path,
    workflow: &Workflow,
    trigger: &str,
    event_paths: Option<&[PathBuf]>,
    cancelled: &AtomicBool,
    execution_lock: &Mutex<()>,
    app: &Option<AppHandle>,
    initial_context: Option<&str>,
) {
    let Ok(_guard) = execution_lock.lock() else { return };
    cancelled.store(false, Ordering::Relaxed);
    let attempts = retry_count(workflow);
    let mut final_error = None;
    for attempt in 0..attempts {
        match engine::execute(workflow, false, db_path, trigger, event_paths, true, cancelled, initial_context) {
            Ok(result) => {
                if let Some(app) = app {
                    crate::send_notifications(app, &result);
                }
                return;
            }
            Err(error) => {
                if error == "Run stopped by user" {
                    return;
                }
                final_error = Some(error);
                if attempt + 1 < attempts {
                    thread::sleep(Duration::from_millis(250 * 2_u64.pow(attempt as u32)));
                }
            }
        }
    }
    if let Some(error) = final_error {
        let _ = database::record_failure(db_path, workflow, trigger, &error);
    }
}

fn set_error(health: &Arc<Mutex<WorkerHealth>>, error: String) {
    if let Ok(mut value) = health.lock() {
        value.last_error = Some(error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{NodeData, WorkflowEdge, WorkflowNode};
    use std::sync::atomic::AtomicBool;

    #[test]
    fn five_field_schedule_matches() {
        let now = Local::now();
        let expression = format!("{} {} * * {}", now.minute(), now.hour(), now.weekday().num_days_from_sunday());
        assert!(cron_matches(&expression, &now));
        assert!(!cron_matches("99 99 * * *", &now));
    }

    #[test]
    fn native_folder_event_runs_an_enabled_workflow() {
        let directory = std::env::temp_dir().join(format!("conduit-watch-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let database_path = directory.join("conduit.sqlite3");
        database::initialize(&database_path).unwrap();
        let workflow = Workflow {
            id: "watch-test".into(),
            name: "Watch test".into(),
            enabled: false,
            permissions: vec![directory.display().to_string()],
            nodes: vec![
                WorkflowNode {
                    id: "trigger".into(),
                    data: NodeData {
                        label: "Folder changed".into(),
                        kind: "trigger".into(),
                        node_type: "folder_changed".into(),
                        config: HashMap::from([(
                            "path".into(),
                            serde_json::Value::String(directory.display().to_string()),
                        )]),
                    },
                },
                WorkflowNode {
                    id: "condition".into(),
                    data: NodeData {
                        label: "Text files".into(),
                        kind: "condition".into(),
                        node_type: "extension".into(),
                        config: HashMap::from([(
                            "extension".into(),
                            serde_json::Value::String(".txt".into()),
                        )]),
                    },
                },
                WorkflowNode {
                    id: "action".into(),
                    data: NodeData {
                        label: "Checksum".into(),
                        kind: "action".into(),
                        node_type: "checksum".into(),
                        config: HashMap::new(),
                    },
                },
            ],
            edges: vec![
                WorkflowEdge { source: "trigger".into(), target: "condition".into() },
                WorkflowEdge { source: "condition".into(), target: "action".into() },
            ],
        };
        database::approve_preview(&database_path, &workflow).unwrap();
        database::set_enabled(&database_path, &workflow, true).unwrap();

        let health = Arc::new(Mutex::new(WorkerHealth::default()));
        let shutdown = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::new(AtomicBool::new(false));
        let execution_lock = Arc::new(Mutex::new(()));
        spawn(database_path.clone(), health, shutdown.clone(), cancelled, execution_lock, None);
        thread::sleep(Duration::from_secs(1));
        let source = directory.join("sample.txt");
        std::fs::write(&source, b"background engine").unwrap();

        let sidecar = directory.join("sample.txt.sha256");
        for _ in 0..40 {
            if sidecar.exists() {
                break;
            }
            thread::sleep(Duration::from_millis(200));
        }
        shutdown.store(true, Ordering::Relaxed);
        assert!(sidecar.exists(), "the native folder event did not run the workflow");
        assert!(database::list_runs(&database_path).unwrap().iter().any(|run| run.status == "success"));
        thread::sleep(Duration::from_millis(600));
        let _ = std::fs::remove_dir_all(directory);
    }
}
