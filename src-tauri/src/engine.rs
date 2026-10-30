use crate::{actions, database, downloads, removable};
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

pub(crate) enum ProgressEvent {
    RemovableTransfer { bytes: u64 },
    ArchiveExtraction { source: String, destination: String },
}

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
    let now = chrono::Local::now();
    let date = now.format("%Y-%m-%d").to_string();
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
        .replace("{year}", &now.format("%Y").to_string())
        .replace("{month}", &now.format("%m").to_string())
        .replace("{name}", &name)
        .replace("{stem}", &stem)
        .replace("{ext}", &extension)
}

fn apply_form_values(template: &str, workflow: &Workflow) -> String {
    let mut output = template.to_owned();
    if let Some(trigger) = trigger_node(workflow) {
        for (key, value) in &trigger.data.config {
            if let Some(field) = key.strip_prefix("form.") {
                if let Some(value) = value.as_str() {
                    output = output.replace(&format!("{{form.{field}}}"), value);
                }
            }
        }
    }
    output
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
    let organizes_downloads = workflow
        .nodes
        .iter()
        .any(|node| node.data.node_type == "organize_downloads");
    let explicit = workflow
        .nodes
        .iter()
        .filter(|node| {
            matches!(
                node.data.node_type.as_str(),
                "copy" | "move" | "rename" | "checksum" | "trash" | "compress" | "extract" |
                "media_convert" | "ocr" | "document_metadata" | "organize_downloads" | "file_inventory"
            )
        })
        .find_map(|node| string_config(node, "source"));
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
        if path.is_file() {
            vec![path]
        } else if path.is_dir() {
            fs::read_dir(&path)
                .map_err(|error| format!("Cannot read {}: {error}", path.display()))?
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|entry| entry.is_file() || (organizes_downloads && entry.is_dir()))
                .take(max_files * 2)
                .collect()
        } else {
            Vec::new()
        }
    } else if let Some(paths) = event_paths {
        paths
            .iter()
            .filter(|path| path.is_file() || (organizes_downloads && path.is_dir()))
            .map(|path| assert_allowed(path, permissions))
            .collect::<Result<Vec<_>, _>>()?
    } else if let Some(folder) = folder {
        let directory = assert_allowed(&expand_home(&folder)?, permissions)?;
        fs::read_dir(&directory)
            .map_err(|error| format!("Cannot read {}: {error}", directory.display()))?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_file() || (organizes_downloads && path.is_dir()))
            .take(max_files * 2)
            .collect()
    } else {
        Vec::new()
    };

    candidates = candidates.into_iter().map(downloads::candidate_path).collect();
    candidates.retain(|file| {
        let incomplete_download = organizes_downloads && downloads::is_incomplete_download(file);
        !incomplete_download
            && condition_matches(workflow, file)
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
    ordered: &[&WorkflowNode],
    files: &[PathBuf],
    permissions: &[PathBuf],
) -> Result<Vec<PreviewOperation>, String> {
    let mut operations = Vec::new();
    let mut routed: HashMap<PathBuf, PathBuf> = HashMap::new();
    for node in ordered.iter().copied().filter(|node| node.data.kind == "action") {
        let destination = string_config(node, "destination")
            .map(|raw| expand_home(&expand_template(&raw, None)))
            .transpose()?
            .map(|path| assert_allowed(&path, permissions))
            .transpose()?;
        let destination = if node.data.node_type == "write_text" {
            destination.map(collision_safe)
        } else {
            destination
        };
        let configured_source = string_config(node, "source")
            .or_else(|| string_config(node, "repository"))
            .map(|raw| expand_home(&raw))
            .transpose()?
            .map(|path| assert_allowed(&path, permissions))
            .transpose()?;
        if node.data.node_type == "sync_removable_library" {
            let packages = string_config(node, "packagesSource").ok_or("Removable-library sync needs a PS4 package source")?;
            assert_allowed(&expand_home(&packages)?, permissions)?;
        }
        let uses_files = matches!(
            node.data.node_type.as_str(),
            "copy" | "move" | "rename" | "checksum" | "trash" | "compress" | "extract" |
            "media_convert" | "ocr" | "document_metadata" | "organize_downloads" | "file_inventory"
        );
        let sources: Vec<PathBuf> = if uses_files && !files.is_empty() {
            files.to_vec()
        } else {
            configured_source.clone().into_iter().collect()
        };
        if uses_files && !sources.is_empty() {
            for original_source in &sources {
                let source = routed.get(original_source).cloned().unwrap_or_else(|| original_source.clone());
                let mut action = node.data.node_type.clone();
                let target = match node.data.node_type.as_str() {
                    "copy" | "move" => destination.as_ref().map(|directory| {
                        let name = string_config(node, "template")
                            .map(|template| expand_template(&template, Some(&source)))
                            .filter(|value| !value.is_empty())
                            .unwrap_or_else(|| source.file_name().unwrap_or_default().to_string_lossy().into_owned());
                        collision_safe(directory.join(name))
                    }),
                    "rename" => source.parent().map(|directory| {
                        let name = string_config(node, "template")
                            .map(|template| expand_template(&template, Some(&source)))
                            .filter(|value| !value.is_empty())
                            .unwrap_or_else(|| source.file_name().unwrap_or_default().to_string_lossy().into_owned());
                        collision_safe(directory.join(name))
                    }),
                    "media_convert" => destination.as_ref().map(|directory| {
                        let extension = string_config(node, "outputExtension").unwrap_or_else(|| "webp".into());
                        collision_safe(directory.join(source.file_stem().unwrap_or_default()).with_extension(extension.trim_start_matches('.')))
                    }),
                    "compress" => destination.clone().map(collision_safe),
                    "extract" => destination.clone(),
                    "checksum" => Some(collision_safe(PathBuf::from(format!("{}.sha256", source.display())))),
                    "organize_downloads" => match downloads::organization(&source, node)? {
                        Some(downloads::Organization::Move(directory)) => {
                            Some(collision_safe(assert_allowed(&directory, permissions)?.join(source.file_name().unwrap_or_default())))
                        }
                        Some(downloads::Organization::ExtractRom(directory)) => {
                            action = "extract_rom_archive".into();
                            Some(assert_allowed(&directory, permissions)?)
                        }
                        Some(downloads::Organization::ExtractPackage(directory)) => {
                            action = "extract_package_archive".into();
                            Some(assert_allowed(&directory, permissions)?)
                        }
                        None => None,
                    },
                    _ => None,
                };
                if node.data.node_type == "organize_downloads" && target.is_none() {
                    continue;
                }
                if matches!(node.data.node_type.as_str(), "copy" | "move" | "rename" | "media_convert" | "compress" | "organize_downloads") {
                    if let Some(target) = &target {
                        routed.insert(original_source.clone(), target.clone());
                        routed.insert(source.clone(), target.clone());
                    }
                }
                operations.push(PreviewOperation {
                    node_id: node.id.clone(),
                    node_label: node.data.label.clone(),
                    action: action.clone(),
                    source: Some(source.display().to_string()),
                    destination: target.map(|path| path.display().to_string()),
                    warning: match action.as_str() {
                        "move" | "organize_downloads" => Some("Original file will be moved".into()),
                        "extract_rom_archive" => Some("ROMs will be extracted and the source archive deleted after success".into()),
                        "extract_package_archive" => {
                            Some("PS4 package files will be extracted and the source archive deleted after success".into())
                        }
                        "trash" => Some("File will be sent to desktop trash".into()),
                        "http_request" => Some("External effects cannot be undone".into()),
                        _ => None,
                    },
                });
            }
        } else if !uses_files {
            operations.push(PreviewOperation {
                node_id: node.id.clone(),
                node_label: node.data.label.clone(),
                action: node.data.node_type.clone(),
                source: configured_source.map(|path| path.display().to_string()),
                destination: if node.data.node_type == "notification" {
                    string_config(node, "message").map(|message| apply_form_values(&message, workflow))
                } else {
                    destination.map(|path| path.display().to_string())
                },
                warning: match node.data.node_type.as_str() {
                    "http_request" => Some("External effects cannot be undone".into()),
                    "kmail_cleanup" if string_config(node, "mode").as_deref() == Some("preview_and_apply") => {
                        Some("Creates a fresh preview, then moves those messages to Cleanup Review; nothing is deleted".into())
                    }
                    "kmail_cleanup" if string_config(node, "mode").as_deref() == Some("apply_reviewed") => {
                        Some("Moves only the latest KMail preview to Cleanup Review; mail is not deleted".into())
                    }
                    "kmail_cleanup" => Some("Creates a review report; no mail is moved".into()),
                    _ => None,
                },
            });
        }
    }
    let has_download_organizer = workflow
        .nodes
        .iter()
        .any(|node| node.data.node_type == "organize_downloads");
    let organized_download = operations
        .iter()
        .any(|operation| {
            matches!(
                operation.action.as_str(),
                "organize_downloads" | "extract_rom_archive" | "extract_package_archive"
            )
        });
    if has_download_organizer && !organized_download {
        operations.retain(|operation| operation.action != "notification");
    }
    Ok(operations)
}

fn perform_operations(
    operations: &mut [PreviewOperation],
    workflow: &Workflow,
    cancelled: &AtomicBool,
    db_path: &Path,
    run_id: &str,
    initial_context: Option<&str>,
    on_progress: Option<&dyn Fn(ProgressEvent)>,
) -> Result<(), String> {
    let mut context = initial_context.unwrap_or_default().to_owned();
    for (sequence, operation) in operations.iter_mut().enumerate() {
        if cancelled.load(Ordering::Relaxed) {
            return Err("Run stopped by user".into());
        }
        let node = workflow
            .nodes
            .iter()
            .find(|node| node.id == operation.node_id)
            .ok_or("An action node no longer exists")?;
        if operation.action == "notification" {
            operation.destination = operation.destination.as_ref().map(|message| message.replace("{input}", &context));
        }
        if actions::run(operation, node, &mut context)? {
            if matches!(operation.action.as_str(), "compress" | "media_convert") {
                database::record_operation(
                    db_path,
                    run_id,
                    sequence,
                    &operation.action,
                    operation.source.as_deref(),
                    operation.destination.as_deref(),
                )?;
            }
            continue;
        }
        match operation.action.as_str() {
            "sync_removable_library" => {
                let source = Path::new(operation.source.as_deref().ok_or("Removable-library sync needs a ROM source")?);
                let destination = Path::new(operation.destination.as_deref().ok_or("Removable-library sync needs an SSD destination")?);
                let mounted = initial_context.ok_or("Removable-library sync runs only after its SSD is mounted")?;
                if Path::new(mounted).canonicalize().map_err(|error| error.to_string())?
                    != destination.canonicalize().map_err(|error| error.to_string())?
                {
                    return Err("The mounted drive does not match the configured SSD destination".into());
                }
                let packages = string_config(node, "packagesSource").ok_or("Removable-library sync needs a PS4 package source")?;
                let packages = expand_home(&packages)?;
                let announce = |bytes| {
                    if let Some(notify) = on_progress {
                        notify(ProgressEvent::RemovableTransfer { bytes });
                    }
                };
                let summary = removable::sync_library(source, &packages, destination, cancelled, Some(&announce))?;
                context = format!(
                    "Copied or updated {} ROM game{} ({} file{}), left {} already-current game{} unchanged, and moved {} PS4 package{}.",
                    summary.updated_rom_items,
                    if summary.updated_rom_items == 1 { "" } else { "s" },
                    summary.copied_rom_files,
                    if summary.copied_rom_files == 1 { "" } else { "s" },
                    summary.unchanged_rom_items,
                    if summary.unchanged_rom_items == 1 { "" } else { "s" },
                    summary.moved_packages,
                    if summary.moved_packages == 1 { "" } else { "s" }
                );
            }
            "extract_rom_archive" => {
                if let (Some(source), Some(destination)) = (&operation.source, &operation.destination) {
                    if let Some(notify) = on_progress {
                        notify(ProgressEvent::ArchiveExtraction {
                            source: source.clone(),
                            destination: destination.clone(),
                        });
                    }
                    downloads::extract_rom_archive(Path::new(source), Path::new(destination))?;
                }
            }
            "extract_package_archive" => {
                if let (Some(source), Some(destination)) = (&operation.source, &operation.destination) {
                    if let Some(notify) = on_progress {
                        notify(ProgressEvent::ArchiveExtraction {
                            source: source.clone(),
                            destination: destination.clone(),
                        });
                    }
                    downloads::extract_package_archive(Path::new(source), Path::new(destination))?;
                }
            }
            "create_directory" => {
                if let Some(destination) = &operation.destination {
                    let created = !Path::new(destination).exists();
                    fs::create_dir_all(destination).map_err(|error| error.to_string())?;
                    if created {
                        database::record_operation(db_path, run_id, sequence, "create_directory", None, Some(destination))?;
                    }
                }
            }
            "copy" => {
                if let (Some(source), Some(destination)) = (&operation.source, &operation.destination) {
                    if let Some(parent) = Path::new(destination).parent() {
                        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                    }
                    fs::copy(source, destination).map_err(|error| error.to_string())?;
                    database::record_operation(db_path, run_id, sequence, "copy", Some(source), Some(destination))?;
                }
            }
            "move" | "organize_downloads" => {
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
                    database::record_operation(db_path, run_id, sequence, &operation.action, Some(source), Some(destination))?;
                }
            }
            "rename" => {
                if let (Some(source), Some(destination)) = (&operation.source, &operation.destination) {
                    fs::rename(source, destination).map_err(|error| error.to_string())?;
                    database::record_operation(db_path, run_id, sequence, "rename", Some(source), Some(destination))?;
                }
            }
            "write_text" => {
                if let Some(destination) = &operation.destination {
                    let content = string_config(node, "content").unwrap_or_else(|| {
                        format!(
                            "# {}\n\nCreated by Conduit on {}.\n",
                            workflow.name,
                            Utc::now().format("%Y-%m-%d")
                        )
                    });
                    let content = expand_template(&content, None);
                    let content = apply_form_values(&content, workflow).replace("{input}", &context);
                    if let Some(parent) = Path::new(destination).parent() {
                        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                    }
                    fs::write(destination, content).map_err(|error| error.to_string())?;
                    database::record_operation(db_path, run_id, sequence, "write_text", None, Some(destination))?;
                }
            }
            "checksum" => {
                if let (Some(source), Some(destination)) = (&operation.source, &operation.destination) {
                    let bytes = fs::read(source).map_err(|error| error.to_string())?;
                    let digest = format!("{:x}", Sha256::digest(bytes));
                    fs::write(destination, format!("{digest}  {source}\n"))
                        .map_err(|error| error.to_string())?;
                    database::record_operation(db_path, run_id, sequence, "checksum", Some(source), Some(destination))?;
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
    initial_context: Option<&str>,
    on_progress: Option<&dyn Fn(ProgressEvent)>,
) -> Result<EngineResult, String> {
    let started = Instant::now();
    let ordered = validate_graph(workflow)?;
    if workflow.nodes.iter().any(|node| {
        node.data.node_type == "kmail_cleanup"
            && string_config(node, "mode").as_deref() == Some("apply_reviewed")
    }) && trigger_node(workflow).is_none_or(|node| node.data.node_type != "manual") {
        return Err("Applying a KMail cleanup report is allowed only from a manual workflow".into());
    }
    let permissions: Vec<PathBuf> = workflow
        .permissions
        .iter()
        .map(|raw| expand_home(raw).and_then(|path| comparable_path(&path)))
        .collect::<Result<_, _>>()?;
    if permissions.is_empty()
        && workflow.nodes.iter().any(|node| {
            matches!(
                node.data.node_type.as_str(),
                "copy" | "move" | "rename" | "write_text" | "create_directory" | "checksum" |
                "trash" | "compress" | "extract" | "media_convert" | "git_summary" | "ocr" |
                "document_metadata" | "organize_downloads" | "file_inventory" | "kmail_cleanup" |
                "sync_removable_library"
            )
        })
    {
        return Err("Add at least one folder permission before using filesystem actions".into());
    }
    let files = candidate_files(workflow, &permissions, event_paths, db_path, background)?;
    let mut operations = plan_operations(workflow, &ordered, &files, &permissions)?;

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
        reversible: !dry_run && operations.iter().all(|operation| {
            matches!(operation.action.as_str(), "copy" | "move" | "rename" | "organize_downloads" | "create_directory" | "write_text" | "checksum" | "compress" | "media_convert")
        }),
        message: String::new(),
    };
    database::begin_run(db_path, &run, trigger)?;

    if !dry_run {
        if let Err(error) = perform_operations(
            &mut operations,
            workflow,
            cancelled,
            db_path,
            &run.id,
            initial_context,
            on_progress,
        ) {
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
        "create_directory", "copy", "move", "rename", "write_text", "checksum", "notification", "trash",
        "compress", "extract", "http_request", "media_convert", "git_summary", "ocr", "document_metadata",
        "organize_downloads", "file_inventory", "kmail_cleanup", "sync_removable_library",
        "extract_rom_archive", "extract_package_archive",
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

    #[test]
    fn text_templates_expand_calendar_placeholders() {
        let rendered = expand_template("# Week of {date} ({year}-{month})", None);
        assert!(!rendered.contains("{date}"));
        assert!(!rendered.contains("{year}"));
        assert!(!rendered.contains("{month}"));
    }

    #[test]
    fn empty_download_organizer_plans_no_moves_or_notification() {
        let workflow = Workflow {
            id: "empty-downloads".into(),
            name: "Empty downloads".into(),
            enabled: true,
            permissions: vec![],
            nodes: vec![
                WorkflowNode {
                    id: "trigger".into(),
                    data: crate::models::NodeData {
                        label: "Downloads changed".into(),
                        kind: "trigger".into(),
                        node_type: "folder_changed".into(),
                        config: HashMap::new(),
                    },
                },
                WorkflowNode {
                    id: "organize".into(),
                    data: crate::models::NodeData {
                        label: "Organize".into(),
                        kind: "action".into(),
                        node_type: "organize_downloads".into(),
                        config: HashMap::new(),
                    },
                },
                WorkflowNode {
                    id: "notify".into(),
                    data: crate::models::NodeData {
                        label: "Done".into(),
                        kind: "action".into(),
                        node_type: "notification".into(),
                        config: HashMap::from([(
                            "message".into(),
                            serde_json::Value::String("Downloads organized".into()),
                        )]),
                    },
                },
            ],
            edges: vec![
                crate::models::WorkflowEdge { source: "trigger".into(), target: "organize".into() },
                crate::models::WorkflowEdge { source: "organize".into(), target: "notify".into() },
            ],
        };
        let ordered = validate_graph(&workflow).unwrap();
        let operations = plan_operations(&workflow, &ordered, &[], &[]).unwrap();
        assert!(operations.is_empty());
    }

    #[test]
    fn kmail_apply_cannot_be_scheduled() {
        let directory = std::env::temp_dir().join(format!("conduit-mail-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        let database_path = directory.join("runtime.sqlite3");
        database::initialize(&database_path).unwrap();
        let mut workflow = Workflow {
            id: "unsafe-mail-schedule".into(),
            name: "Unsafe mail schedule".into(),
            enabled: false,
            permissions: vec![directory.display().to_string()],
            nodes: vec![
                WorkflowNode {
                    id: "trigger".into(),
                    data: crate::models::NodeData {
                        label: "Schedule".into(), kind: "trigger".into(), node_type: "schedule".into(),
                        config: HashMap::from([("schedule".into(), Value::String("0 18 * * 0".into()))]),
                    },
                },
                WorkflowNode {
                    id: "action".into(),
                    data: crate::models::NodeData {
                        label: "Apply mail".into(), kind: "action".into(), node_type: "kmail_cleanup".into(),
                        config: HashMap::from([
                            ("source".into(), Value::String(directory.display().to_string())),
                            ("mode".into(), Value::String("apply_reviewed".into())),
                        ]),
                    },
                },
            ],
            edges: vec![crate::models::WorkflowEdge { source: "trigger".into(), target: "action".into() }],
        };
        let result = execute(
            &workflow,
            true,
            &database_path,
            "manual",
            None,
            false,
            &AtomicBool::new(false),
            None,
            None,
        );
        assert_eq!(result.unwrap_err(), "Applying a KMail cleanup report is allowed only from a manual workflow");
        workflow.nodes[1]
            .data
            .config
            .insert("mode".into(), Value::String("preview_and_apply".into()));
        let automatic = execute(
            &workflow,
            true,
            &database_path,
            "schedule",
            None,
            true,
            &AtomicBool::new(false),
            None,
            None,
        );
        assert!(automatic.is_ok());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn discovery_is_bounded_for_large_folders() {
        let directory = std::env::temp_dir().join(format!("conduit-large-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        for index in 0..520 {
            fs::write(directory.join(format!("{index:04}.txt")), b"test").unwrap();
        }
        let database_path = directory.join("runtime.sqlite3");
        database::initialize(&database_path).unwrap();
        let workflow = Workflow {
            id: "large-folder".into(),
            name: "Large folder".into(),
            enabled: false,
            permissions: vec![directory.display().to_string()],
            nodes: vec![
                WorkflowNode {
                    id: "trigger".into(),
                    data: crate::models::NodeData {
                        label: "Folder".into(), kind: "trigger".into(), node_type: "folder_changed".into(),
                        config: HashMap::from([("path".into(), Value::String(directory.display().to_string()))]),
                    },
                },
                WorkflowNode {
                    id: "action".into(),
                    data: crate::models::NodeData {
                        label: "Checksum".into(), kind: "action".into(), node_type: "checksum".into(),
                        config: HashMap::from([("maxFiles".into(), Value::from(500))]),
                    },
                },
            ],
            edges: vec![crate::models::WorkflowEdge { source: "trigger".into(), target: "action".into() }],
        };
        let permissions = vec![directory.canonicalize().unwrap()];
        let files = candidate_files(&workflow, &permissions, None, &database_path, false).unwrap();
        assert_eq!(files.len(), 500);
        fs::remove_dir_all(directory).unwrap();
    }
}
