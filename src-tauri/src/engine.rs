use crate::database;
use crate::models::{EngineResult, PreviewOperation, RunRecord, Workflow, WorkflowNode};
use chrono::Utc;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};
use uuid::Uuid;

pub(crate) fn workflow_signature(workflow: &Workflow) -> Result<String, String> {
    let material = serde_json::json!({
        "id": workflow.id,
        "name": workflow.name,
        "permissions": workflow.permissions,
        "nodes": workflow.nodes,
        "edges": workflow.edges,
    });
    let bytes = serde_json::to_vec(&material).map_err(|error| error.to_string())?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

pub(crate) fn validate_graph(workflow: &Workflow) -> Result<Vec<&WorkflowNode>, String> {
    let ids: HashSet<&str> = workflow.nodes.iter().map(|node| node.id.as_str()).collect();
    if workflow.nodes.iter().filter(|node| node.data.kind == "trigger").count() != 1 {
        return Err("A workflow must have exactly one trigger".into());
    }
    if !workflow.nodes.iter().any(|node| node.data.kind == "action") {
        return Err("A workflow must have at least one action".into());
    }
    if workflow
        .edges
        .iter()
        .any(|edge| !ids.contains(edge.source.as_str()) || !ids.contains(edge.target.as_str()))
    {
        return Err("A connection references a missing node".into());
    }

    let mut indegree: HashMap<&str, usize> = ids.iter().map(|id| (*id, 0)).collect();
    for edge in &workflow.edges {
        *indegree.entry(edge.target.as_str()).or_default() += 1;
    }
    let mut ready: Vec<&str> = indegree
        .iter()
        .filter_map(|(id, degree)| (*degree == 0).then_some(*id))
        .collect();
    let mut ordered = Vec::new();
    while let Some(id) = ready.pop() {
        if let Some(node) = workflow.nodes.iter().find(|node| node.id == id) {
            ordered.push(node);
        }
        for edge in workflow.edges.iter().filter(|edge| edge.source == id) {
            if let Some(degree) = indegree.get_mut(edge.target.as_str()) {
                *degree -= 1;
                if *degree == 0 {
                    ready.push(edge.target.as_str());
                }
            }
        }
    }
    if ordered.len() != workflow.nodes.len() {
        return Err("Cycles are not allowed".into());
    }
    Ok(ordered)
}

pub(crate) fn trigger_node(workflow: &Workflow) -> Option<&WorkflowNode> {
    workflow.nodes.iter().find(|node| node.data.kind == "trigger")
}

pub(crate) fn string_config(node: &WorkflowNode, key: &str) -> Option<String> {
    node.data
        .config
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

pub(crate) fn number_config(node: &WorkflowNode, key: &str) -> Option<f64> {
    node.data.config.get(key).and_then(Value::as_f64)
}

pub(crate) fn expand_home(raw: &str) -> Result<PathBuf, String> {
    if raw == "~" {
        return dirs::home_dir().ok_or("Home directory is unavailable".into());
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return dirs::home_dir()
            .map(|home| home.join(rest))
            .ok_or("Home directory is unavailable".into());
    }
    Ok(PathBuf::from(raw))
}

fn comparable_path(path: &Path) -> Result<PathBuf, String> {
    if path.exists() {
        return path.canonicalize().map_err(|error| error.to_string());
    }
    let mut existing = path;
    let mut suffix = Vec::new();
    while !existing.exists() {
        let name = existing
            .file_name()
            .ok_or_else(|| format!("Cannot resolve {}", path.display()))?;
        suffix.push(name.to_os_string());
        existing = existing
            .parent()
            .ok_or_else(|| format!("Cannot resolve {}", path.display()))?;
    }
    let mut resolved = existing.canonicalize().map_err(|error| error.to_string())?;
    for part in suffix.into_iter().rev() {
        resolved.push(part);
    }
    Ok(resolved)
}

fn assert_allowed(path: &Path, permissions: &[PathBuf]) -> Result<PathBuf, String> {
    let resolved = comparable_path(path)?;
    if permissions.iter().any(|permission| resolved.starts_with(permission)) {
        Ok(resolved)
    } else {
        Err(format!(
            "Access denied: {} is outside this workflow's folder permissions",
            path.display()
        ))
    }
}

fn expand_template(template: &str, source: Option<&Path>) -> String {
    let date = Utc::now().format("%Y-%m-%d").to_string();
    let name = source
        .and_then(Path::file_name)
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = source
        .and_then(Path::file_stem)
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_default();
    let extension = source
        .and_then(Path::extension)
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_default();
    template
        .replace("{date}", &date)
        .replace("{name}", &name)
        .replace("{stem}", &stem)
        .replace("{ext}", &extension)
}

fn condition_matches(workflow: &Workflow, file: &Path) -> bool {
    workflow.nodes.iter().filter(|node| node.data.kind == "condition").all(|node| {
        match node.data.node_type.as_str() {
            "extension" => string_config(node, "extension").is_none_or(|wanted| {
                file.extension()
                    .map(|extension| format!(".{}", extension.to_string_lossy()).eq_ignore_ascii_case(&wanted))
                    .unwrap_or(false)
            }),
            "file_size" => number_config(node, "maxMb").is_none_or(|max_mb| {
                file.metadata().map(|metadata| metadata.len() as f64 <= max_mb * 1_048_576.0).unwrap_or(false)
            }),
            "age" => number_config(node, "hours").is_none_or(|hours| {
                file.metadata()
                    .and_then(|metadata| metadata.modified())
                    .ok()
                    .and_then(|modified| modified.elapsed().ok())
                    .is_some_and(|elapsed| elapsed.as_secs_f64() >= hours * 3600.0)
            }),
            "exists" => string_config(node, "path")
                .and_then(|path| expand_home(&path).ok())
                .is_some_and(|path| path.exists()),
            _ => true,
        }
    })
}

fn candidate_files(
    workflow: &Workflow,
    permissions: &[PathBuf],
    event_paths: Option<&[PathBuf]>,
    db_path: &Path,
    background: bool,
) -> Result<Vec<PathBuf>, String> {
    let explicit = workflow.nodes.iter().find_map(|node| string_config(node, "source"));
    let folder = trigger_node(workflow)
        .filter(|node| node.data.node_type == "folder_changed")
        .and_then(|node| string_config(node, "path"));
    let max_files = workflow
        .nodes
        .iter()
        .filter_map(|node| node.data.config.get("maxFiles").and_then(Value::as_u64))
        .min()
        .unwrap_or(100)
        .min(500) as usize;

    let mut candidates = if let Some(source) = explicit {
        let path = assert_allowed(&expand_home(&source)?, permissions)?;
        path.is_file().then_some(path).into_iter().collect()
    } else if let Some(paths) = event_paths {
        paths
            .iter()
            .filter(|path| path.is_file())
            .map(|path| assert_allowed(path, permissions))
            .collect::<Result<Vec<_>, _>>()?
    } else if let Some(folder) = folder {
        let directory = assert_allowed(&expand_home(&folder)?, permissions)?;
        fs::read_dir(&directory)
            .map_err(|error| format!("Cannot read {}: {error}", directory.display()))?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_file())
            .take(max_files * 2)
            .collect()
    } else {
        Vec::new()
    };

    candidates.retain(|file| {
        condition_matches(workflow, file)
            && (!background || !database::was_processed(db_path, &workflow.id, file).unwrap_or(false))
    });
    candidates.sort();
    candidates.dedup();
    candidates.truncate(max_files);
    Ok(candidates)
}

fn collision_safe(destination: PathBuf) -> PathBuf {
    if !destination.exists() {
        return destination;
    }
    let stem = destination.file_stem().unwrap_or_default().to_string_lossy();
    let extension = destination.extension().map(|value| value.to_string_lossy().into_owned());
    for number in 2..10_000 {
        let name = match &extension {
            Some(extension) => format!("{stem}-{number}.{extension}"),
            None => format!("{stem}-{number}"),
        };
        let candidate = destination.with_file_name(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    destination
}

fn plan_operations(
    workflow: &Workflow,
    files: &[PathBuf],
    permissions: &[PathBuf],
) -> Result<Vec<PreviewOperation>, String> {
    let mut operations = Vec::new();
    for node in workflow.nodes.iter().filter(|node| node.data.kind == "action") {
        let destination = string_config(node, "destination")
            .map(|raw| expand_home(&expand_template(&raw, None)))
            .transpose()?
            .map(|path| assert_allowed(&path, permissions))
            .transpose()?;
        if matches!(node.data.node_type.as_str(), "copy" | "move" | "rename" | "checksum")
            && !files.is_empty()
        {
            for source in files {
                let target_directory = if node.data.node_type == "rename" {
                    source.parent()
                } else {
                    destination.as_deref()
                };
                let target = target_directory.map(|directory| {
                    let name = string_config(node, "template")
                        .map(|template| expand_template(&template, Some(source)))
                        .filter(|value| !value.is_empty())
                        .unwrap_or_else(|| source.file_name().unwrap_or_default().to_string_lossy().into_owned());
                    collision_safe(directory.join(name))
                });
                operations.push(PreviewOperation {
                    node_id: node.id.clone(),
                    node_label: node.data.label.clone(),
                    action: node.data.node_type.clone(),
                    source: Some(source.display().to_string()),
                    destination: target.map(|path| path.display().to_string()),
                    warning: (node.data.node_type == "move").then(|| "Original file will be moved".into()),
                });
            }
        } else {
            operations.push(PreviewOperation {
                node_id: node.id.clone(),
                node_label: node.data.label.clone(),
                action: node.data.node_type.clone(),
                source: None,
                destination: destination.map(|path| path.display().to_string()),
                warning: None,
            });
        }
    }
    Ok(operations)
}

fn perform_operations(
    operations: &[PreviewOperation],
    workflow: &Workflow,
    cancelled: &AtomicBool,
) -> Result<(), String> {
    for operation in operations {
        if cancelled.load(Ordering::Relaxed) {
            return Err("Run stopped by user".into());
        }
        match operation.action.as_str() {
            "create_directory" => {
                if let Some(destination) = &operation.destination {
                    fs::create_dir_all(destination).map_err(|error| error.to_string())?;
                }
            }
            "copy" => {
                if let (Some(source), Some(destination)) = (&operation.source, &operation.destination) {
                    if let Some(parent) = Path::new(destination).parent() {
                        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                    }
                    fs::copy(source, destination).map_err(|error| error.to_string())?;
                }
            }
            "move" => {
                if let (Some(source), Some(destination)) = (&operation.source, &operation.destination) {
                    if let Some(parent) = Path::new(destination).parent() {
                        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                    }
                    fs::rename(source, destination)
                        .or_else(|_| {
                            fs::copy(source, destination)?;
                            fs::remove_file(source)
                        })
                        .map_err(|error| error.to_string())?;
                }
            }
            "rename" => {
                if let (Some(source), Some(destination)) = (&operation.source, &operation.destination) {
                    fs::rename(source, destination).map_err(|error| error.to_string())?;
                }
            }
            "write_text" => {
                if let Some(destination) = &operation.destination {
                    let node = workflow
                        .nodes
                        .iter()
                        .find(|node| node.id == operation.node_id)
                        .ok_or("The text action no longer exists")?;
                    let content = string_config(node, "content").unwrap_or_else(|| {
                        format!(
                            "# {}\n\nCreated by Conduit on {}.\n",
                            workflow.name,
                            Utc::now().format("%Y-%m-%d")
                        )
                    });
                    if let Some(parent) = Path::new(destination).parent() {
                        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                    }
                    fs::write(destination, content).map_err(|error| error.to_string())?;
                }
            }
            "checksum" => {
                if let Some(source) = &operation.source {
                    let bytes = fs::read(source).map_err(|error| error.to_string())?;
                    let digest = format!("{:x}", Sha256::digest(bytes));
                    fs::write(format!("{source}.sha256"), format!("{digest}  {source}\n"))
                        .map_err(|error| error.to_string())?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

pub(crate) fn execute(
    workflow: &Workflow,
    dry_run: bool,
    db_path: &Path,
    trigger: &str,
    event_paths: Option<&[PathBuf]>,
    background: bool,
    cancelled: &AtomicBool,
) -> Result<EngineResult, String> {
    let started = Instant::now();
    let ordered = validate_graph(workflow)?;
    let permissions: Vec<PathBuf> = workflow
        .permissions
        .iter()
        .map(|raw| expand_home(raw).and_then(|path| comparable_path(&path)))
        .collect::<Result<_, _>>()?;
    if permissions.is_empty()
        && workflow.nodes.iter().any(|node| {
            matches!(
                node.data.node_type.as_str(),
                "copy" | "move" | "rename" | "write_text" | "create_directory" | "checksum"
            )
        })
    {
        return Err("Add at least one folder permission before using filesystem actions".into());
    }
    let files = candidate_files(workflow, &permissions, event_paths, db_path, background)?;
    let operations = plan_operations(workflow, &files, &permissions)?;

    if dry_run {
        database::approve_preview(db_path, workflow)?;
    } else if !database::is_approved(db_path, workflow)? {
        return Err("This workflow changed or has not been previewed. Run a dry run first.".into());
    }

    let mut run = RunRecord {
        id: Uuid::new_v4().to_string(),
        workflow_id: workflow.id.clone(),
        workflow_name: workflow.name.clone(),
        started_at: Utc::now().to_rfc3339(),
        duration_ms: 0,
        status: if dry_run { "preview".into() } else { "running".into() },
        files: files.len(),
        reversible: operations.iter().all(|operation| {
            matches!(operation.action.as_str(), "copy" | "move" | "rename" | "create_directory")
        }),
        message: String::new(),
    };
    database::begin_run(db_path, &run, trigger)?;

    if !dry_run {
        if let Err(error) = perform_operations(&operations, workflow, cancelled) {
            run.status = if error == "Run stopped by user" { "stopped".into() } else { "failed".into() };
            run.duration_ms = started.elapsed().as_millis();
            run.message = format!("{trigger}: {error}");
            database::finish_run(db_path, &run)?;
            return Err(error);
        }
        if background {
            let mut processed = files.clone();
            for operation in &operations {
                if let Some(destination) = &operation.destination {
                    processed.push(PathBuf::from(destination));
                }
                if operation.action == "checksum" {
                    if let Some(source) = &operation.source {
                        processed.push(PathBuf::from(format!("{source}.sha256")));
                    }
                }
            }
            processed.sort();
            processed.dedup();
            database::mark_processed(db_path, &workflow.id, &processed)?;
        }
    }

    run.duration_ms = started.elapsed().as_millis();
    run.status = if dry_run { "preview".into() } else { "success".into() };
    run.message = if dry_run {
        format!(
            "{} action{} checked against {} file{}",
            operations.len(),
            if operations.len() == 1 { "" } else { "s" },
            files.len(),
            if files.len() == 1 { "" } else { "s" }
        )
    } else {
        format!(
            "{trigger}: completed {} planned action{}",
            operations.len(),
            if operations.len() == 1 { "" } else { "s" }
        )
    };
    database::finish_run(db_path, &run)?;

    let supported = [
        "manual", "folder_changed", "schedule", "startup", "extension", "file_size", "age", "exists",
        "create_directory", "copy", "move", "rename", "write_text", "checksum",
    ];
    let node_statuses = ordered
        .into_iter()
        .map(|node| {
            (
                node.id.clone(),
                if supported.contains(&node.data.node_type.as_str()) {
                    "success".into()
                } else {
                    "skipped".into()
                },
            )
        })
        .collect();
    Ok(EngineResult { run, operations, node_statuses })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collision_names_do_not_overwrite() {
        let directory = std::env::temp_dir().join(format!("conduit-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        let original = directory.join("report.pdf");
        fs::write(&original, b"one").unwrap();
        assert_eq!(collision_safe(original), directory.join("report-2.pdf"));
        fs::remove_dir_all(directory).unwrap();
    }
}
