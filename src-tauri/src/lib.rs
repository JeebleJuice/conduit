use chrono::Utc;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{collections::{HashMap, HashSet}, fs, hash::{DefaultHasher, Hash, Hasher}, path::{Path, PathBuf}, sync::Mutex, time::Instant};
use tauri::{Manager, State};
use uuid::Uuid;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Workflow {
    id: String,
    name: String,
    permissions: Vec<String>,
    nodes: Vec<WorkflowNode>,
    edges: Vec<WorkflowEdge>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct WorkflowNode { id: String, data: NodeData }

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct NodeData {
    label: String,
    kind: String,
    node_type: String,
    config: HashMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkflowEdge { source: String, target: String }

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PreviewOperation {
    node_id: String,
    node_label: String,
    action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    destination: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    warning: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct RunRecord {
    id: String,
    workflow_id: String,
    workflow_name: String,
    started_at: String,
    duration_ms: u128,
    status: String,
    files: usize,
    reversible: bool,
    message: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngineResult {
    run: RunRecord,
    operations: Vec<PreviewOperation>,
    node_statuses: HashMap<String, String>,
}

struct AppState {
    db_path: PathBuf,
    approved_previews: Mutex<HashMap<String, u64>>,
}

fn expand_home(raw: &str) -> Result<PathBuf, String> {
    if raw == "~" { return dirs::home_dir().ok_or("Home directory is unavailable".into()); }
    if let Some(rest) = raw.strip_prefix("~/") {
        return dirs::home_dir().map(|home| home.join(rest)).ok_or("Home directory is unavailable".into());
    }
    Ok(PathBuf::from(raw))
}

fn comparable_path(path: &Path) -> Result<PathBuf, String> {
    if path.exists() { return path.canonicalize().map_err(|error| error.to_string()); }
    let mut existing = path;
    let mut suffix = Vec::new();
    while !existing.exists() {
        let name = existing.file_name().ok_or_else(|| format!("Cannot resolve {}", path.display()))?;
        suffix.push(name.to_os_string());
        existing = existing.parent().ok_or_else(|| format!("Cannot resolve {}", path.display()))?;
    }
    let mut resolved = existing.canonicalize().map_err(|error| error.to_string())?;
    for part in suffix.into_iter().rev() { resolved.push(part); }
    Ok(resolved)
}

fn assert_allowed(path: &Path, permissions: &[PathBuf]) -> Result<PathBuf, String> {
    let resolved = comparable_path(path)?;
    if permissions.iter().any(|permission| resolved.starts_with(permission)) { Ok(resolved) }
    else { Err(format!("Access denied: {} is outside this workflow's folder permissions", path.display())) }
}

fn workflow_signature(workflow: &Workflow) -> u64 {
    let mut hasher = DefaultHasher::new();
    serde_json::to_string(workflow).unwrap_or_default().hash(&mut hasher);
    hasher.finish()
}

fn validate_graph(workflow: &Workflow) -> Result<Vec<&WorkflowNode>, String> {
    let ids: HashSet<&str> = workflow.nodes.iter().map(|node| node.id.as_str()).collect();
    if workflow.nodes.iter().filter(|node| node.data.kind == "trigger").count() != 1 { return Err("A workflow must have exactly one trigger".into()); }
    if workflow.edges.iter().any(|edge| !ids.contains(edge.source.as_str()) || !ids.contains(edge.target.as_str())) { return Err("A connection references a missing node".into()); }
    let mut indegree: HashMap<&str, usize> = ids.iter().map(|id| (*id, 0)).collect();
    for edge in &workflow.edges { *indegree.entry(edge.target.as_str()).or_default() += 1; }
    let mut ready: Vec<&str> = indegree.iter().filter_map(|(id, degree)| (*degree == 0).then_some(*id)).collect();
    let mut ordered = Vec::new();
    while let Some(id) = ready.pop() {
        if let Some(node) = workflow.nodes.iter().find(|node| node.id == id) { ordered.push(node); }
        for edge in workflow.edges.iter().filter(|edge| edge.source == id) {
            if let Some(degree) = indegree.get_mut(edge.target.as_str()) { *degree -= 1; if *degree == 0 { ready.push(edge.target.as_str()); } }
        }
    }
    if ordered.len() != workflow.nodes.len() { return Err("Cycles are not allowed".into()); }
    Ok(ordered)
}

fn string_config(node: &WorkflowNode, key: &str) -> Option<String> {
    node.data.config.get(key).and_then(Value::as_str).filter(|value| !value.is_empty()).map(str::to_owned)
}

fn expand_template(template: &str, source: Option<&Path>) -> String {
    let date = Utc::now().format("%Y-%m-%d").to_string();
    let name = source.and_then(Path::file_name).map(|value| value.to_string_lossy().into_owned()).unwrap_or_default();
    let stem = source.and_then(Path::file_stem).map(|value| value.to_string_lossy().into_owned()).unwrap_or_default();
    let extension = source.and_then(Path::extension).map(|value| value.to_string_lossy().into_owned()).unwrap_or_default();
    template.replace("{date}", &date).replace("{name}", &name).replace("{stem}", &stem).replace("{ext}", &extension)
}

fn candidate_files(workflow: &Workflow, permissions: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let folder = workflow.nodes.iter().find(|node| node.data.node_type == "folder_changed").and_then(|node| string_config(node, "path"));
    let explicit = workflow.nodes.iter().find_map(|node| string_config(node, "source"));
    let extension = workflow.nodes.iter().find(|node| node.data.node_type == "extension").and_then(|node| string_config(node, "extension"));
    let max_files = workflow.nodes.iter().filter_map(|node| node.data.config.get("maxFiles").and_then(Value::as_u64)).min().unwrap_or(100).min(500) as usize;
    if let Some(source) = explicit {
        let path = assert_allowed(&expand_home(&source)?, permissions)?;
        return Ok(path.is_file().then_some(path).into_iter().collect());
    }
    let Some(folder) = folder else { return Ok(Vec::new()); };
    let directory = assert_allowed(&expand_home(&folder)?, permissions)?;
    let mut files = Vec::new();
    for entry in fs::read_dir(&directory).map_err(|error| format!("Cannot read {}: {error}", directory.display()))? {
        let path = entry.map_err(|error| error.to_string())?.path();
        let matches = extension.as_ref().map(|wanted| path.extension().map(|ext| format!(".{}", ext.to_string_lossy()).eq_ignore_ascii_case(wanted)).unwrap_or(false)).unwrap_or(true);
        if path.is_file() && matches { files.push(path); if files.len() >= max_files { break; } }
    }
    Ok(files)
}

fn collision_safe(destination: PathBuf) -> PathBuf {
    if !destination.exists() { return destination; }
    let stem = destination.file_stem().unwrap_or_default().to_string_lossy();
    let extension = destination.extension().map(|value| value.to_string_lossy().into_owned());
    for number in 2..10_000 {
        let name = match &extension { Some(ext) => format!("{stem}-{number}.{ext}"), None => format!("{stem}-{number}") };
        let candidate = destination.with_file_name(name);
        if !candidate.exists() { return candidate; }
    }
    destination
}

fn plan_operations(workflow: &Workflow, files: &[PathBuf], permissions: &[PathBuf]) -> Result<Vec<PreviewOperation>, String> {
    let mut operations = Vec::new();
    for node in workflow.nodes.iter().filter(|node| node.data.kind == "action") {
        let destination = string_config(node, "destination").map(|raw| expand_home(&expand_template(&raw, None))).transpose()?.map(|path| assert_allowed(&path, permissions)).transpose()?;
        if matches!(node.data.node_type.as_str(), "copy" | "move" | "rename" | "checksum") && !files.is_empty() {
            for source in files {
                let target_directory = if node.data.node_type == "rename" { source.parent() } else { destination.as_deref() };
                let target = target_directory.map(|directory| {
                    let configured = string_config(node, "template");
                    let name = configured.map(|template| expand_template(&template, Some(source))).filter(|value| !value.is_empty()).unwrap_or_else(|| source.file_name().unwrap_or_default().to_string_lossy().into_owned());
                    collision_safe(directory.join(name))
                });
                operations.push(PreviewOperation { node_id: node.id.clone(), node_label: node.data.label.clone(), action: node.data.node_type.clone(), source: Some(source.display().to_string()), destination: target.map(|path| path.display().to_string()), warning: (node.data.node_type == "move").then(|| "Original file will be moved".into()) });
            }
        } else {
            operations.push(PreviewOperation { node_id: node.id.clone(), node_label: node.data.label.clone(), action: node.data.node_type.clone(), source: None, destination: destination.map(|path| path.display().to_string()), warning: None });
        }
    }
    Ok(operations)
}

fn perform_operations(operations: &[PreviewOperation], workflow: &Workflow) -> Result<(), String> {
    for operation in operations {
        match operation.action.as_str() {
            "create_directory" => if let Some(destination) = &operation.destination { fs::create_dir_all(destination).map_err(|error| error.to_string())?; },
            "copy" => if let (Some(source), Some(destination)) = (&operation.source, &operation.destination) { if let Some(parent) = Path::new(destination).parent() { fs::create_dir_all(parent).map_err(|error| error.to_string())?; } fs::copy(source, destination).map_err(|error| error.to_string())?; },
            "move" => if let (Some(source), Some(destination)) = (&operation.source, &operation.destination) { if let Some(parent) = Path::new(destination).parent() { fs::create_dir_all(parent).map_err(|error| error.to_string())?; } fs::rename(source, destination).or_else(|_| { fs::copy(source, destination)?; fs::remove_file(source) }).map_err(|error| error.to_string())?; },
            "rename" => if let (Some(source), Some(destination)) = (&operation.source, &operation.destination) { fs::rename(source, destination).map_err(|error| error.to_string())?; },
            "write_text" => if let Some(destination) = &operation.destination { let node = workflow.nodes.iter().find(|node| node.id == operation.node_id).unwrap(); let content = string_config(node, "content").unwrap_or_else(|| format!("# {}\n\nCreated by Conduit on {}.\n", workflow.name, Utc::now().format("%Y-%m-%d"))); if let Some(parent) = Path::new(destination).parent() { fs::create_dir_all(parent).map_err(|error| error.to_string())?; } fs::write(destination, content).map_err(|error| error.to_string())?; },
            "checksum" => if let Some(source) = &operation.source { let bytes = fs::read(source).map_err(|error| error.to_string())?; let _digest = format!("{:x}", Sha256::digest(bytes)); },
            _ => {}
        }
    }
    Ok(())
}

fn persist_run(db_path: &Path, run: &RunRecord) -> Result<(), String> {
    let connection = Connection::open(db_path).map_err(|error| error.to_string())?;
    connection.execute("CREATE TABLE IF NOT EXISTS runs (id TEXT PRIMARY KEY, workflow_id TEXT NOT NULL, workflow_name TEXT NOT NULL, started_at TEXT NOT NULL, duration_ms INTEGER NOT NULL, status TEXT NOT NULL, files INTEGER NOT NULL, reversible INTEGER NOT NULL, message TEXT NOT NULL)", []).map_err(|error| error.to_string())?;
    connection.execute("INSERT INTO runs VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)", params![run.id, run.workflow_id, run.workflow_name, run.started_at, run.duration_ms as i64, run.status, run.files as i64, run.reversible, run.message]).map_err(|error| error.to_string())?;
    Ok(())
}

#[tauri::command]
fn execute_workflow(workflow: Workflow, dry_run: bool, state: State<AppState>) -> Result<EngineResult, String> {
    let started = Instant::now();
    let ordered = validate_graph(&workflow)?;
    let permissions: Vec<PathBuf> = workflow.permissions.iter().map(|raw| expand_home(raw).and_then(|path| comparable_path(&path))).collect::<Result<_, _>>()?;
    if permissions.is_empty() && workflow.nodes.iter().any(|node| matches!(node.data.node_type.as_str(), "copy" | "move" | "rename" | "write_text" | "create_directory")) { return Err("Add at least one folder permission before using filesystem actions".into()); }
    let files = candidate_files(&workflow, &permissions)?;
    let operations = plan_operations(&workflow, &files, &permissions)?;
    let signature = workflow_signature(&workflow);
    if dry_run { state.approved_previews.lock().map_err(|_| "Preview state is unavailable")?.insert(workflow.id.clone(), signature); }
    else {
        let approved = state.approved_previews.lock().map_err(|_| "Preview state is unavailable")?.get(&workflow.id).copied() == Some(signature);
        if !approved { return Err("This workflow changed or has not been previewed. Run a dry run first.".into()); }
        perform_operations(&operations, &workflow)?;
    }
    let run = RunRecord { id: Uuid::new_v4().to_string(), workflow_id: workflow.id.clone(), workflow_name: workflow.name.clone(), started_at: Utc::now().to_rfc3339(), duration_ms: started.elapsed().as_millis(), status: if dry_run { "preview".into() } else { "success".into() }, files: files.len(), reversible: operations.iter().all(|operation| matches!(operation.action.as_str(), "copy" | "move" | "rename" | "create_directory")), message: if dry_run { format!("{} action{} checked against {} file{}", operations.len(), if operations.len() == 1 { "" } else { "s" }, files.len(), if files.len() == 1 { "" } else { "s" }) } else { format!("Completed {} planned action{}", operations.len(), if operations.len() == 1 { "" } else { "s" }) } };
    persist_run(&state.db_path, &run)?;
    let supported = ["manual", "folder_changed", "schedule", "startup", "extension", "file_size", "age", "exists", "create_directory", "copy", "move", "rename", "write_text", "checksum"];
    let node_statuses = ordered.into_iter().map(|node| (node.id.clone(), if supported.contains(&node.data.node_type.as_str()) { "success".into() } else { "skipped".into() })).collect();
    Ok(EngineResult { run, operations, node_statuses })
}

#[tauri::command]
fn list_run_history(state: State<AppState>) -> Result<Vec<RunRecord>, String> {
    let connection = Connection::open(&state.db_path).map_err(|error| error.to_string())?;
    connection.execute("CREATE TABLE IF NOT EXISTS runs (id TEXT PRIMARY KEY, workflow_id TEXT NOT NULL, workflow_name TEXT NOT NULL, started_at TEXT NOT NULL, duration_ms INTEGER NOT NULL, status TEXT NOT NULL, files INTEGER NOT NULL, reversible INTEGER NOT NULL, message TEXT NOT NULL)", []).map_err(|error| error.to_string())?;
    let mut statement = connection.prepare("SELECT id,workflow_id,workflow_name,started_at,duration_ms,status,files,reversible,message FROM runs ORDER BY started_at DESC LIMIT 100").map_err(|error| error.to_string())?;
    let rows = statement.query_map([], |row| Ok(RunRecord { id: row.get(0)?, workflow_id: row.get(1)?, workflow_name: row.get(2)?, started_at: row.get(3)?, duration_ms: row.get::<_, i64>(4)? as u128, status: row.get(5)?, files: row.get::<_, i64>(6)? as usize, reversible: row.get(7)?, message: row.get(8)? })).map_err(|error| error.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|error| error.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            fs::create_dir_all(&data_dir)?;
            app.manage(AppState { db_path: data_dir.join("conduit.sqlite3"), approved_previews: Mutex::new(HashMap::new()) });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![execute_workflow, list_run_history])
        .run(tauri::generate_context!())
        .expect("error while running Conduit");
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn collision_names_do_not_overwrite() {
        let dir = std::env::temp_dir().join(format!("conduit-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let original = dir.join("report.pdf");
        fs::write(&original, b"one").unwrap();
        assert_eq!(collision_safe(original), dir.join("report-2.pdf"));
        fs::remove_dir_all(dir).unwrap();
    }
}
