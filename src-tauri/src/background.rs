use crate::{
    database, downloads, removable,
    engine::{self, expand_home, number_config, string_config, trigger_node},
    models::{WorkerHealth, Workflow},
};

struct PendingDriveRetry {
    mount_path: PathBuf,
    last_available: u64,
    space_increased_at: Option<Instant>,
}

impl PendingDriveRetry {
    fn observe_available_space(&mut self, available: u64, now: Instant) -> bool {
        if available > self.last_available {
            self.last_available = available;
            self.space_increased_at = Some(now);
        } else if available < self.last_available {
            self.last_available = available;
            self.space_increased_at = None;
        }
        self.space_increased_at
            .is_some_and(|changed| now.duration_since(changed) >= Duration::from_secs(3))
    }
}
use chrono::{Datelike, Local, Timelike, Utc};
use notify::{event::{ModifyKind, RenameMode}, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tauri::AppHandle;
use tauri_plugin_notification::NotificationExt;
use std::{
    collections::{HashMap, HashSet},
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::Command,
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
    let mut fingerprints: HashMap<String, u64> = HashMap::new();
    let mut last_refresh = Instant::now() - Duration::from_secs(30);
    let mut last_reconcile = Instant::now() - Duration::from_secs(60);
    let mut cooldowns: HashMap<String, Instant> = HashMap::new();
    let mut pending_folder_events: HashMap<String, HashMap<PathBuf, Instant>> = HashMap::new();
    let mut webhooks: HashMap<String, TcpListener> = HashMap::new();
    let mut mounted = HashSet::new();
    let mut pending_drive_retries: HashMap<String, PendingDriveRetry> = HashMap::new();
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
                            let _ = run_with_retries(db_path, workflow, "startup", None, cancelled, execution_lock, app, None);
                        }
                    }
                }
                Err(error) => set_error(health, error),
            }
            last_refresh = Instant::now();
        }

        for workflow in &workflows {
            if let Some(slot) = due_schedule_slot(workflow, &Local::now()) {
                if database::claim_schedule(db_path, &workflow.id, &slot).unwrap_or(false) {
                    let _ = run_with_retries(db_path, workflow, "schedule", None, cancelled, execution_lock, app, None);
                }
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
                    let _ = run_with_retries(db_path, workflow, "local webhook", None, cancelled, execution_lock, app, Some(body));
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
                    let matches_uuid = string_config(trigger, "mountUuid")
                        .is_none_or(|wanted| mount_uuid(path).is_some_and(|actual| actual.eq_ignore_ascii_case(&wanted)));
                    if matches_name && matches_uuid && !recently_ran(&mut cooldowns, &workflow.id) {
                        let mounted_path = path.display().to_string();
                        match run_with_retries(
                            db_path,
                            workflow,
                            "drive mounted",
                            None,
                            cancelled,
                            execution_lock,
                            app,
                            Some(&mounted_path),
                        ) {
                            Err(error) if removable::is_insufficient_space_error(&error) => {
                                pending_drive_retries.insert(
                                    workflow.id.clone(),
                                    PendingDriveRetry {
                                        mount_path: path.clone(),
                                        last_available: removable::available_space(path).unwrap_or_default(),
                                        space_increased_at: None,
                                    },
                                );
                            }
                            _ => {
                                pending_drive_retries.remove(&workflow.id);
                            }
                        }
                    }
                }
            }
            mounted = current;

            pending_drive_retries.retain(|_, pending| mounted.contains(&pending.mount_path));
            let now = Instant::now();
            let mut ready = Vec::new();
            for (workflow_id, pending) in &mut pending_drive_retries {
                let Some(available) = removable::available_space(&pending.mount_path) else { continue };
                if pending.observe_available_space(available, now) {
                    ready.push((workflow_id.clone(), pending.mount_path.clone()));
                }
            }
            for (workflow_id, mount_path) in ready {
                let Some(workflow) = workflows.iter().find(|workflow| workflow.id == workflow_id) else {
                    pending_drive_retries.remove(&workflow_id);
                    continue;
                };
                let mounted_path = mount_path.display().to_string();
                match run_with_retries(
                    db_path,
                    workflow,
                    "drive space available",
                    None,
                    cancelled,
                    execution_lock,
                    app,
                    Some(&mounted_path),
                ) {
                    Err(error) if removable::is_insufficient_space_error(&error) => {
                        if let Some(pending) = pending_drive_retries.get_mut(&workflow_id) {
                            pending.last_available = removable::available_space(&mount_path).unwrap_or(pending.last_available);
                            pending.space_increased_at = None;
                        }
                    }
                    _ => {
                        pending_drive_retries.remove(&workflow_id);
                    }
                }
            }
            last_mount_check = Instant::now();
        }

        match event_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(Ok(event)) if is_relevant_folder_event(&event) => {
                for workflow in &workflows {
                    let Some(folder) = watched_folder(workflow) else { continue };
                    let pending = pending_folder_events.entry(workflow.id.clone()).or_default();
                    for path in event.paths.iter().filter(|path| path.starts_with(&folder)) {
                        pending.insert(path.clone(), Instant::now());
                    }
                }
            }
            Ok(Ok(_)) => {}
            Ok(Err(error)) => set_error(health, format!("Filesystem watcher: {error}")),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                set_error(health, "Filesystem watcher stopped unexpectedly".into());
                watcher = None;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }

        let mut settled = Vec::new();
        pending_folder_events.retain(|workflow_id, paths| {
            let ready: Vec<PathBuf> = paths
                .iter()
                .filter_map(|(path, changed_at)| (changed_at.elapsed() >= Duration::from_secs(3)).then_some(path.clone()))
                .collect();
            for path in &ready {
                paths.remove(path);
            }
            if !ready.is_empty() {
                settled.push((workflow_id.clone(), ready));
            }
            !paths.is_empty()
        });
        for (workflow_id, paths) in settled {
            if let Some(workflow) = workflows.iter().find(|workflow| workflow.id == workflow_id) {
                let organizes_downloads = workflow
                    .nodes
                    .iter()
                    .any(|node| node.data.node_type == "organize_downloads");
                let ready: Vec<PathBuf> = paths
                    .into_iter()
                    .filter(|path| {
                        (path.is_file() || (organizes_downloads && path.is_dir()))
                            && (!organizes_downloads || !downloads::is_incomplete_download(path))
                    })
                    .collect();
                if !ready.is_empty() {
                    let _ = run_with_retries(
                        db_path,
                        workflow,
                        "completed folder change",
                        Some(&ready),
                        cancelled,
                        execution_lock,
                        app,
                        None,
                    );
                }
            }
        }

        if last_reconcile.elapsed() >= Duration::from_secs(60) {
            for workflow in &workflows {
                let Some(folder) = watched_folder(workflow) else { continue };
                let current = folder_fingerprint(&folder);
                let changed = fingerprints
                    .insert(workflow.id.clone(), current)
                    .is_none_or(|previous| previous != current)
                    || (workflow.nodes.iter().any(|node| node.data.node_type == "organize_downloads")
                        && downloads::has_organizable_directory(&folder));
                if changed && !recently_ran(&mut cooldowns, &workflow.id) {
                    let _ = run_with_retries(db_path, workflow, "reconciliation", None, cancelled, execution_lock, app, None);
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

#[cfg(target_os = "linux")]
fn mount_uuid(path: &Path) -> Option<String> {
    let output = Command::new("findmnt")
        .args(["--noheadings", "--output", "UUID", "--target"])
        .arg(path)
        .output()
        .ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned()).filter(|uuid| !uuid.is_empty())
}

#[cfg(not(target_os = "linux"))]
fn mount_uuid(_path: &Path) -> Option<String> {
    None
}

fn is_relevant_folder_event(event: &Event) -> bool {
    matches!(
        event.kind,
        EventKind::Create(_)
            | EventKind::Modify(ModifyKind::Data(_))
            | EventKind::Modify(ModifyKind::Name(RenameMode::To | RenameMode::Both | RenameMode::Any))
    )
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

fn due_schedule_slot(workflow: &Workflow, now: &chrono::DateTime<Local>) -> Option<String> {
    let Some(trigger) = trigger_node(workflow).filter(|node| node.data.node_type == "schedule") else {
        return None;
    };
    if let Some(interval) = number_config(trigger, "intervalMinutes") {
        let minute = now.timestamp().div_euclid(60);
        return (interval >= 1.0 && minute % interval.round() as i64 == 0)
            .then(|| now.format("%Y-%m-%dT%H:%M").to_string());
    }
    let schedule = string_config(trigger, "schedule")?;
    let catch_up = number_config(trigger, "catchUpMinutes")
        .unwrap_or(0.0)
        .round()
        .clamp(0.0, 10_080.0) as i64;
    (0..=catch_up).find_map(|offset| {
        let candidate = *now - chrono::Duration::minutes(offset);
        cron_matches(&schedule, &candidate)
            .then(|| candidate.format("%Y-%m-%dT%H:%M").to_string())
    })
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
) -> Result<(), String> {
    let Ok(_guard) = execution_lock.lock() else { return Err("Background execution lock is unavailable".into()) };
    cancelled.store(false, Ordering::Relaxed);
    let attempts = retry_count(workflow);
    let mut final_error = None;
    for attempt in 0..attempts {
        let progress_notification = |event| {
            if let Some(app) = app {
                let (title, body) = match event {
                    engine::ProgressEvent::RemovableTransfer { bytes } => (
                        "PS4USB transfer started",
                        format!("Conduit is transferring {} of new ROMs and PS4 packages to the SSD.", format_bytes(bytes)),
                    ),
                    engine::ProgressEvent::ArchiveExtraction { source, destination } => {
                        let name = Path::new(&source).file_name().unwrap_or_default().to_string_lossy();
                        (
                            "Archive extraction started",
                            format!("{name} → {destination}. Conduit will notify you when it finishes."),
                        )
                    }
                };
                let _ = app.notification().builder().title(title).body(body).show();
            }
        };
        match engine::execute(
            workflow,
            false,
            db_path,
            trigger,
            event_paths,
            true,
            cancelled,
            initial_context,
            Some(&progress_notification),
        ) {
            Ok(result) => {
                if let Some(app) = app {
                    crate::send_notifications(app, &result);
                }
                return Ok(());
            }
            Err(error) => {
                if error == "Run stopped by user" {
                    return Err(error);
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
        if workflow.id == "ps4usb-library-sync" {
            if let Some(app) = app {
                let (title, body) = if let Some(bytes) = removable::insufficient_space_needed(&error) {
                    (
                        "PS4USB sync waiting for space",
                        format!(
                            "The SSD needs {} more. Delete installed packages while it remains connected; Conduit will retry automatically.",
                            format_bytes(bytes)
                        ),
                    )
                } else {
                    ("PS4USB sync failed", error.clone())
                };
                let _ = app.notification().builder().title(title).body(body).show();
            }
        }
        Err(error)
    } else {
        Ok(())
    }
}

fn format_bytes(bytes: u64) -> String {
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.1} GiB", bytes as f64 / GIB)
    } else if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / MIB)
    } else {
        format!("{bytes} bytes")
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
    fn folder_watcher_ignores_reads_but_accepts_new_or_completed_files() {
        assert!(!is_relevant_folder_event(&Event::new(EventKind::Access(
            notify::event::AccessKind::Close(notify::event::AccessMode::Read),
        ))));
        assert!(is_relevant_folder_event(&Event::new(EventKind::Create(
            notify::event::CreateKind::File,
        ))));
        assert!(is_relevant_folder_event(&Event::new(EventKind::Modify(
            ModifyKind::Name(notify::event::RenameMode::To),
        ))));
    }

    #[test]
    fn pending_drive_retry_waits_for_freed_space_to_settle() {
        let started = Instant::now();
        let mut pending = PendingDriveRetry {
            mount_path: PathBuf::from("/drive"),
            last_available: 100,
            space_increased_at: None,
        };

        assert!(!pending.observe_available_space(200, started));
        assert!(!pending.observe_available_space(200, started + Duration::from_secs(2)));
        assert!(pending.observe_available_space(200, started + Duration::from_secs(3)));

        assert!(!pending.observe_available_space(150, started + Duration::from_secs(4)));
        assert_eq!(pending.space_increased_at, None);
    }

    #[test]
    fn scheduled_workflow_can_catch_up_after_downtime() {
        let now = Local::now();
        let five_minutes_ago = now - chrono::Duration::minutes(5);
        let workflow = Workflow {
            id: "catch-up".into(),
            name: "Catch up".into(),
            enabled: true,
            permissions: vec![],
            nodes: vec![WorkflowNode {
                id: "trigger".into(),
                data: NodeData {
                    label: "Schedule".into(),
                    kind: "trigger".into(),
                    node_type: "schedule".into(),
                    config: HashMap::from([
                        ("schedule".into(), serde_json::Value::String(format!(
                            "{} {} * * *",
                            five_minutes_ago.minute(),
                            five_minutes_ago.hour()
                        ))),
                        ("catchUpMinutes".into(), serde_json::Value::from(10)),
                    ]),
                },
            }],
            edges: vec![],
        };
        assert_eq!(
            due_schedule_slot(&workflow, &now),
            Some(five_minutes_ago.format("%Y-%m-%dT%H:%M").to_string())
        );
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
