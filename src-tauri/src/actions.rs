use crate::{database, models::{CapabilityStatus, PreviewOperation, ReversibleOperation, WorkflowNode}};
use reqwest::blocking::Client;
use std::{
    fs,
    io::Read,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::Duration,
};
use zip::{write::SimpleFileOptions, ZipArchive, ZipWriter};

pub(crate) fn capability_statuses() -> Vec<CapabilityStatus> {
    let mut statuses: Vec<CapabilityStatus> = [
        ("git", "Git summaries", "git", "--version"),
        ("ffmpeg", "Media conversion", "ffmpeg", "-version"),
        ("tesseract", "OCR", "tesseract", "--version"),
        ("exiftool", "Document metadata", "exiftool", "-ver"),
    ]
    .into_iter()
    .map(|(id, label, command, version_arg)| {
        let output = Command::new(command).arg(version_arg).stdout(Stdio::piped()).stderr(Stdio::piped()).output();
        match output {
            Ok(output) if output.status.success() => CapabilityStatus {
                id: id.into(),
                label: label.into(),
                available: true,
                detail: first_line(&String::from_utf8_lossy(&output.stdout)),
            },
            _ => CapabilityStatus {
                id: id.into(),
                label: label.into(),
                available: false,
                detail: format!("Install `{command}` to enable this node"),
            },
        }
    })
    .collect();
    let cleanup_root = dirs::home_dir().map(|home| home.join("Dev/kmail-cleanup"));
    let cleanup = cleanup_root.as_ref().map(|root| root.join("sort_inbox.py"));
    let mover = cleanup_root.as_ref().map(|root| root.join("kmail_cleanup_move"));
    let missing: Vec<&str> = [
        (!cleanup.as_ref().is_some_and(|path| path.is_file())).then_some("sort_inbox.py"),
        (!mover.as_ref().is_some_and(|path| path.is_file())).then_some("kmail_cleanup_move"),
        (!on_path("akonadictl")).then_some("akonadictl"),
        (!on_path("mariadb")).then_some("mariadb"),
    ]
    .into_iter()
    .flatten()
    .collect();
    let available = missing.is_empty();
    statuses.push(CapabilityStatus {
        id: "kmail-cleanup".into(),
        label: "KMail Cleanup".into(),
        available,
        detail: if available {
            cleanup.unwrap().display().to_string()
        } else {
            format!("Missing: {}", missing.join(", "))
        },
    });
    statuses
}

fn on_path(command: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|directory| directory.join(command).is_file())
    })
}

fn expand_user_path(raw: &str) -> Result<std::path::PathBuf, String> {
    if raw == "~" {
        return dirs::home_dir().ok_or_else(|| "Home directory is unavailable".into());
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return dirs::home_dir()
            .map(|home| home.join(rest))
            .ok_or_else(|| "Home directory is unavailable".into());
    }
    Ok(std::path::PathBuf::from(raw))
}

fn first_line(value: &str) -> String {
    value.lines().next().unwrap_or("Available").chars().take(100).collect()
}

pub(crate) fn run(
    operation: &PreviewOperation,
    node: &WorkflowNode,
    context: &mut String,
) -> Result<bool, String> {
    match operation.action.as_str() {
        "trash" => {
            let source = required(&operation.source, "Trash needs a source file")?;
            trash::delete(source).map_err(|error| error.to_string())?;
            Ok(true)
        }
        "compress" => {
            create_zip(
                Path::new(required(&operation.source, "ZIP creation needs a source")?),
                Path::new(required(&operation.destination, "ZIP creation needs a destination")?),
            )?;
            Ok(true)
        }
        "extract" => {
            extract_zip(
                Path::new(required(&operation.source, "ZIP extraction needs a source")?),
                Path::new(required(&operation.destination, "ZIP extraction needs a destination")?),
            )?;
            Ok(true)
        }
        "media_convert" => {
            external(
                "ffmpeg",
                &[
                    "-nostdin",
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-i",
                    required(&operation.source, "Media conversion needs a source")?,
                    "-y",
                    required(&operation.destination, "Media conversion needs a destination")?,
                ],
            )?;
            Ok(true)
        }
        "git_summary" => {
            let repository = required(&operation.source, "Git summary needs a repository")?;
            let since = node
                .data
                .config
                .get("sinceDays")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(7)
                .clamp(1, 365);
            let output = git_workspace_summary(Path::new(repository), since)?;
            append_context(context, &output);
            Ok(true)
        }
        "ocr" => {
            let language = node
                .data
                .config
                .get("language")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("eng");
            let output = external(
                "tesseract",
                &[required(&operation.source, "OCR needs a source")?, "stdout", "-l", language],
            )?;
            append_context(context, &output);
            Ok(true)
        }
        "document_metadata" => {
            let output = external(
                "exiftool",
                &["-json", required(&operation.source, "Metadata extraction needs a source")?],
            )?;
            append_context(context, &output);
            Ok(true)
        }
        "file_inventory" => {
            let source = Path::new(required(&operation.source, "File inventory needs a source")?);
            let metadata = source.metadata().map_err(|error| error.to_string())?;
            let modified = metadata
                .modified()
                .ok()
                .map(chrono::DateTime::<chrono::Local>::from)
                .map(|date| date.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_else(|| "unknown".into());
            append_context(
                context,
                &format!("- `{}` — {} — modified {modified}", source.display(), human_size(metadata.len())),
            );
            Ok(true)
        }
        "kmail_cleanup" => {
            let project = Path::new(required(&operation.source, "KMail Cleanup needs its project directory")?);
            let canonical_project = project.canonicalize().map_err(|error| error.to_string())?;
            let expected_project = dirs::home_dir()
                .map(|home| home.join("Dev/kmail-cleanup"))
                .ok_or("Home directory is unavailable")?
                .canonicalize()
                .map_err(|error| format!("Expected KMail Cleanup project is unavailable: {error}"))?;
            if canonical_project != expected_project {
                return Err("KMail Cleanup is restricted to ~/Dev/kmail-cleanup".into());
            }
            let script = canonical_project.join("sort_inbox.py");
            let canonical_script = script.canonicalize().map_err(|error| format!("KMail Cleanup script is unavailable: {error}"))?;
            if canonical_script.parent() != Some(canonical_project.as_path())
                || canonical_script.file_name().and_then(|name| name.to_str()) != Some("sort_inbox.py")
            {
                return Err("KMail Cleanup script resolved outside its configured project".into());
            }
            let mode = node
                .data
                .config
                .get("mode")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("preview");
            let arguments: Vec<String> = match mode {
                "preview" => Vec::new(),
                "apply_reviewed" => {
                    let runs = canonical_project
                        .join("runs")
                        .canonicalize()
                        .map_err(|error| format!("KMail Cleanup runs directory is unavailable: {error}"))?;
                    let configured_report = node
                        .data
                        .config
                        .get("reportPath")
                        .and_then(serde_json::Value::as_str)
                        .filter(|value| !value.trim().is_empty());
                    let report = if let Some(report) = configured_report {
                        expand_user_path(report)?
                            .canonicalize()
                            .map_err(|error| format!("Cannot open reviewed report: {error}"))?
                    } else {
                        latest_candidates_report(&runs)?
                    };
                    if !report.starts_with(&runs)
                        || report.file_name().and_then(|name| name.to_str()) != Some("candidates.csv")
                    {
                        return Err("The reviewed report must be a candidates.csv inside the KMail Cleanup runs directory".into());
                    }
                    vec!["--apply".into(), "--report".into(), report.display().to_string()]
                }
                _ => return Err("KMail Cleanup mode must be preview or apply_reviewed".into()),
            };
            ensure_akonadi()?;
            let output = external_path(&canonical_script, &arguments)?;
            append_context(context, &output);
            Ok(true)
        }
        "http_request" => {
            let url = node
                .data
                .config
                .get("url")
                .and_then(serde_json::Value::as_str)
                .ok_or("HTTP request needs a URL")?;
            if !(url.starts_with("https://") || url.starts_with("http://127.0.0.1") || url.starts_with("http://localhost")) {
                return Err("HTTP requests require HTTPS, except for loopback addresses".into());
            }
            let method = node
                .data
                .config
                .get("method")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("GET")
                .to_uppercase();
            let body = node
                .data
                .config
                .get("body")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .replace("{input}", context);
            let client = Client::builder()
                .timeout(Duration::from_secs(15))
                .redirect(reqwest::redirect::Policy::limited(3))
                .build()
                .map_err(|error| error.to_string())?;
            let mut request = match method.as_str() {
                "GET" => client.get(url),
                "POST" => client.post(url).body(body),
                "PUT" => client.put(url).body(body),
                "PATCH" => client.patch(url).body(body),
                _ => return Err("HTTP method must be GET, POST, PUT, or PATCH".into()),
            };
            if let Some(secret_name) = node.data.config.get("secretName").and_then(serde_json::Value::as_str).filter(|name| !name.is_empty()) {
                let secret = get_secret(secret_name)?;
                request = request.bearer_auth(secret);
            }
            let response = request.send().map_err(|error| error.to_string())?;
            let status = response.status();
            if response.content_length().is_some_and(|length| length > 1_048_576) {
                return Err("HTTP response exceeded the 1 MB safety limit".into());
            }
            let mut bytes = Vec::new();
            response
                .take(1_048_577)
                .read_to_end(&mut bytes)
                .map_err(|error| error.to_string())?;
            if bytes.len() > 1_048_576 {
                return Err("HTTP response exceeded the 1 MB safety limit".into());
            }
            let text = String::from_utf8_lossy(&bytes);
            append_context(context, &text);
            if !status.is_success() {
                return Err(format!("HTTP request returned {status}"));
            }
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn ensure_akonadi() -> Result<(), String> {
    #[cfg(not(target_os = "linux"))]
    return Err("KMail Cleanup is available only on Linux".into());

    #[cfg(target_os = "linux")]
    {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(format!("/run/user/{}", unsafe { libc::geteuid() })));
        let socket = runtime.join("akonadi/mysql.socket");
        if socket.exists() {
            return Ok(());
        }
        let output = Command::new("akonadictl")
            .arg("start")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .map_err(|error| format!("Cannot start Akonadi: {error}"))?;
        if !output.status.success() {
            return Err(format!("Cannot start Akonadi: {}", String::from_utf8_lossy(&output.stderr).trim()));
        }
        for _ in 0..40 {
            if socket.exists() {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(250));
        }
        Err("Akonadi did not become ready within 10 seconds".into())
    }
}

fn required<'a>(value: &'a Option<String>, message: &str) -> Result<&'a str, String> {
    value.as_deref().ok_or_else(|| message.to_owned())
}

fn append_context(context: &mut String, value: &str) {
    if !context.is_empty() {
        context.push_str("\n\n");
    }
    context.push_str(value.trim());
}

fn human_size(bytes: u64) -> String {
    if bytes >= 1_073_741_824 {
        format!("{:.1} GiB", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_048_576 {
        format!("{:.1} MiB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

fn external(command: &str, arguments: &[&str]) -> Result<String, String> {
    let output = Command::new(command)
        .args(arguments)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                format!("Optional tool `{command}` is not installed")
            } else {
                error.to_string()
            }
        })?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn external_path(command: &Path, arguments: &[String]) -> Result<String, String> {
    let output = Command::new(command)
        .args(arguments)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(if message.is_empty() {
            format!("{} exited with {}", command.display(), output.status)
        } else {
            message
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn git_repositories(root: &Path) -> Result<Vec<std::path::PathBuf>, String> {
    let root = root.canonicalize().map_err(|error| format!("Cannot inspect {}: {error}", root.display()))?;
    if root.join(".git").exists() {
        return Ok(vec![root]);
    }
    if !root.is_dir() {
        return Err(format!("Git workspace is not a directory: {}", root.display()));
    }

    let mut repositories = fs::read_dir(&root)
        .map_err(|error| format!("Cannot inspect {}: {error}", root.display()))?
        .filter_map(Result::ok)
        .filter_map(|entry| entry.path().canonicalize().ok())
        .filter(|path| path.starts_with(&root) && path.is_dir() && path.join(".git").exists())
        .collect::<Vec<_>>();
    repositories.sort();
    repositories.truncate(100);
    Ok(repositories)
}

fn latest_candidates_report(runs: &Path) -> Result<std::path::PathBuf, String> {
    let canonical_runs = runs
        .canonicalize()
        .map_err(|error| format!("KMail Cleanup runs directory is unavailable: {error}"))?;
    let mut reports = fs::read_dir(&canonical_runs)
        .map_err(|error| format!("Cannot inspect KMail Cleanup runs: {error}"))?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| {
            let run_name = entry.file_name();
            let report = entry.path().join("candidates.csv").canonicalize().ok()?;
            (report.starts_with(&canonical_runs)
                && report.is_file()
                && report.file_name().and_then(|name| name.to_str()) == Some("candidates.csv"))
                .then_some((run_name, report))
        })
        .collect::<Vec<_>>();
    reports.sort_by(|left, right| left.0.cmp(&right.0));
    reports
        .pop()
        .map(|(_, report)| report)
        .ok_or_else(|| "No KMail cleanup preview was found. Run the preview workflow first.".into())
}

fn git_command(repository: &Path, arguments: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                "Optional tool `git` is not installed".to_owned()
            } else {
                error.to_string()
            }
        })?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(format!(
            "Cannot summarize {}: {}",
            repository.display(),
            message
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn git_workspace_summary(root: &Path, since_days: u64) -> Result<String, String> {
    let repositories = git_repositories(root)?;
    if repositories.is_empty() {
        return Ok("No Git repositories were found in the projects folder.".into());
    }

    let since = format!("--since={since_days} days ago");
    let mut sections = Vec::new();
    for repository in &repositories {
        let commits = if git_command(repository, &["rev-parse", "--verify", "HEAD"]).is_ok() {
            git_command(
                repository,
                &["log", &since, "--no-merges", "--pretty=format:- %h %s (%an)", "--max-count=30"],
            )?
        } else {
            String::new()
        };
        let status = git_command(repository, &["status", "--short", "--untracked-files=normal"])?;
        if commits.is_empty() && status.is_empty() {
            continue;
        }

        let name = repository.file_name().unwrap_or_default().to_string_lossy();
        let mut section = format!("## {name}\n\nPath: `{}`\n", repository.display());
        if commits.is_empty() {
            section.push_str("\nNo commits in this period.\n");
        } else {
            section.push_str("\n### Commits\n\n");
            section.push_str(&commits);
            section.push('\n');
        }
        if !status.is_empty() {
            section.push_str("\n### Uncommitted changes\n\n```text\n");
            for line in status.lines().take(100) {
                section.push_str(line);
                section.push('\n');
            }
            if status.lines().count() > 100 {
                section.push_str("… additional changes omitted\n");
            }
            section.push_str("```\n");
        }
        sections.push(section);
    }

    if sections.is_empty() {
        Ok(format!(
            "No commits or uncommitted changes were found across {} repositories.",
            repositories.len()
        ))
    } else {
        Ok(sections.join("\n"))
    }
}

fn create_zip(source: &Path, destination: &Path) -> Result<(), String> {
    const MAX_ARCHIVE_BYTES: u64 = 10 * 1024 * 1024 * 1024;
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let file = fs::File::create(destination).map_err(|error| error.to_string())?;
    let mut archive = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    if source.is_file() {
        if source.metadata().map_err(|error| error.to_string())?.len() > MAX_ARCHIVE_BYTES {
            return Err("Archive input exceeded the 10 GB safety limit".into());
        }
        archive.start_file(source.file_name().unwrap_or_default().to_string_lossy(), options).map_err(|error| error.to_string())?;
        let mut input = fs::File::open(source).map_err(|error| error.to_string())?;
        std::io::copy(&mut input, &mut archive).map_err(|error| error.to_string())?;
    } else {
        let mut file_count = 0;
        let mut total_bytes = 0;
        add_directory(&mut archive, source, source, options, &mut file_count, &mut total_bytes)?;
    }
    archive.finish().map_err(|error| error.to_string())?;
    Ok(())
}

fn add_directory(
    archive: &mut ZipWriter<fs::File>,
    root: &Path,
    current: &Path,
    options: SimpleFileOptions,
    file_count: &mut usize,
    total_bytes: &mut u64,
) -> Result<(), String> {
    for entry in fs::read_dir(current).map_err(|error| error.to_string())? {
        let path = entry.map_err(|error| error.to_string())?.path();
        let name = path.strip_prefix(root).map_err(|error| error.to_string())?.to_string_lossy().replace('\\', "/");
        if path.is_dir() {
            add_directory(archive, root, &path, options, file_count, total_bytes)?;
        } else {
            *file_count += 1;
            *total_bytes = total_bytes.saturating_add(path.metadata().map_err(|error| error.to_string())?.len());
            if *file_count > 10_000 || *total_bytes > 10 * 1024 * 1024 * 1024 {
                return Err("Archive input exceeded the 10,000 file or 10 GB safety limit".into());
            }
            archive.start_file(name, options).map_err(|error| error.to_string())?;
            let mut input = fs::File::open(path).map_err(|error| error.to_string())?;
            std::io::copy(&mut input, archive).map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

fn extract_zip(source: &Path, destination: &Path) -> Result<(), String> {
    fs::create_dir_all(destination).map_err(|error| error.to_string())?;
    let file = fs::File::open(source).map_err(|error| error.to_string())?;
    let mut archive = ZipArchive::new(file).map_err(|error| error.to_string())?;
    if archive.len() > 10_000 {
        return Err("Archive exceeded the 10,000 entry safety limit".into());
    }
    let mut total_bytes = 0_u64;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|error| error.to_string())?;
        total_bytes = total_bytes.saturating_add(entry.size());
        if total_bytes > 2 * 1024 * 1024 * 1024 {
            return Err("Extracted data exceeded the 2 GB safety limit".into());
        }
        let relative = entry.enclosed_name().ok_or("Archive contains an unsafe path")?;
        let output = destination.join(relative);
        if entry.is_dir() {
            fs::create_dir_all(&output).map_err(|error| error.to_string())?;
        } else {
            if let Some(parent) = output.parent() {
                fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            let mut file = fs::File::create(&output).map_err(|error| error.to_string())?;
            std::io::copy(&mut entry, &mut file).map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

pub(crate) fn set_secret(name: &str, secret: &str) -> Result<(), String> {
    keyring::Entry::new("dev.jeeblejuice.conduit", name)
        .map_err(|error| error.to_string())?
        .set_password(secret)
        .map_err(|error| error.to_string())
}

fn get_secret(name: &str) -> Result<String, String> {
    keyring::Entry::new("dev.jeeblejuice.conduit", name)
        .map_err(|error| error.to_string())?
        .get_password()
        .map_err(|_| format!("Secret `{name}` was not found in the operating-system keyring"))
}

pub(crate) fn delete_secret(name: &str) -> Result<(), String> {
    keyring::Entry::new("dev.jeeblejuice.conduit", name)
        .map_err(|error| error.to_string())?
        .delete_credential()
        .map_err(|error| error.to_string())
}

pub(crate) fn undo_run(db_path: &Path, run_id: &str) -> Result<usize, String> {
    let operations = database::reversible_operations(db_path, run_id)?;
    let mut undone = 0;
    for operation in operations {
        undo_operation(db_path, run_id, &operation)?;
        undone += 1;
    }
    Ok(undone)
}

fn undo_operation(db_path: &Path, run_id: &str, operation: &ReversibleOperation) -> Result<(), String> {
    match operation.action.as_str() {
        "move" | "rename" | "organize_downloads" => {
            let source = operation.source.as_deref().ok_or("Undo record has no original path")?;
            let destination = operation.destination.as_deref().ok_or("Undo record has no resulting path")?;
            if Path::new(source).exists() {
                return Err(format!("Cannot undo because the original path now exists: {source}"));
            }
            fs::rename(destination, source).map_err(|error| error.to_string())?;
        }
        "copy" | "write_text" | "checksum" | "media_convert" | "compress" => {
            let destination = operation.destination.as_deref().ok_or("Undo record has no resulting path")?;
            if Path::new(destination).exists() {
                trash::delete(destination).map_err(|error| error.to_string())?;
            }
        }
        "create_directory" => {
            let destination = operation.destination.as_deref().ok_or("Undo record has no resulting path")?;
            fs::remove_dir(destination).map_err(|error| format!("Directory is not empty or changed: {error}"))?;
        }
        _ => return Err(format!("{} cannot be undone automatically", operation.action)),
    }
    database::mark_operation_undone(db_path, run_id, operation.sequence)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn zip_extraction_rejects_parent_traversal() {
        let root = std::env::temp_dir().join(format!("conduit-zip-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let archive_path = root.join("unsafe.zip");
        let file = fs::File::create(&archive_path).unwrap();
        let mut archive = ZipWriter::new(file);
        archive.start_file("../escape.txt", SimpleFileOptions::default()).unwrap();
        archive.write_all(b"nope").unwrap();
        archive.finish().unwrap();
        let result = extract_zip(&archive_path, &root.join("output"));
        assert!(result.is_err());
        assert!(!root.join("escape.txt").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn git_workspace_discovers_only_direct_repositories() {
        let root = std::env::temp_dir().join(format!("conduit-git-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("alpha/.git")).unwrap();
        fs::create_dir_all(root.join("ordinary/nested/.git")).unwrap();
        fs::create_dir_all(root.join("beta/.git")).unwrap();
        let repositories = git_repositories(&root).unwrap();
        assert_eq!(repositories.len(), 2);
        assert!(repositories[0].ends_with("alpha"));
        assert!(repositories[1].ends_with("beta"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn git_workspace_reports_uncommitted_work_without_history() {
        let root = std::env::temp_dir().join(format!("conduit-git-summary-test-{}", uuid::Uuid::new_v4()));
        let repository = root.join("new-project");
        fs::create_dir_all(&repository).unwrap();
        assert!(Command::new("git").args(["init", "-q"]).arg(&repository).status().unwrap().success());
        fs::write(repository.join("README.md"), "# New project\n").unwrap();
        let summary = git_workspace_summary(&root, 7).unwrap();
        assert!(summary.contains("## new-project"));
        assert!(summary.contains("README.md"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn latest_kmail_report_uses_newest_run_directory() {
        let root = std::env::temp_dir().join(format!("conduit-mail-report-test-{}", uuid::Uuid::new_v4()));
        let older = root.join("20261014-180000-000001");
        let newer = root.join("20261021-180000-000002");
        fs::create_dir_all(&older).unwrap();
        fs::create_dir_all(&newer).unwrap();
        fs::write(older.join("candidates.csv"), "older").unwrap();
        fs::write(newer.join("candidates.csv"), "newer").unwrap();
        assert_eq!(latest_candidates_report(&root).unwrap(), newer.join("candidates.csv"));
        fs::remove_dir_all(root).unwrap();
    }
}
