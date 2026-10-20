use crate::{database, models::{CapabilityStatus, PreviewOperation, ReversibleOperation, WorkflowNode}};
use reqwest::blocking::Client;
use std::{
    fs,
    io::Read,
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};
use zip::{write::SimpleFileOptions, ZipArchive, ZipWriter};

pub(crate) fn capability_statuses() -> Vec<CapabilityStatus> {
    [
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
    .collect()
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
            let output = external(
                "git",
                &[
                    "-C",
                    repository,
                    "log",
                    &format!("--since={since} days ago"),
                    "--no-merges",
                    "--pretty=format:- %h %s (%an)",
                    "--max-count=200",
                ],
            )?;
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

fn required<'a>(value: &'a Option<String>, message: &str) -> Result<&'a str, String> {
    value.as_deref().ok_or_else(|| message.to_owned())
}

fn append_context(context: &mut String, value: &str) {
    if !context.is_empty() {
        context.push_str("\n\n");
    }
    context.push_str(value.trim());
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
        "move" | "rename" => {
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
}
