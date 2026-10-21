use crate::models::WorkflowNode;
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use zip::ZipArchive;

const SAMPLE_LIMIT: u64 = 256 * 1024;

pub(crate) fn destination(source: &Path, node: &WorkflowNode) -> Result<Option<PathBuf>, String> {
    let extension = source
        .extension()
        .map(|value| value.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if matches!(extension.as_str(), "part" | "crdownload" | "download" | "tmp") {
        return Ok(None);
    }

    let name = source.file_name().unwrap_or_default().to_string_lossy().to_ascii_lowercase();
    let mime = infer::get_from_path(source)
        .map_err(|error| format!("Cannot inspect {}: {error}", source.display()))?
        .map(|kind| kind.mime_type().to_owned());
    let document_like = matches!(
        extension.as_str(),
        "pdf" | "doc" | "docx" | "odt" | "rtf" | "txt" | "md" | "csv" | "xls" | "xlsx" | "ods" | "ppt" | "pptx" | "odp"
    ) || mime.as_deref() == Some("application/pdf");

    if document_like {
        let searchable = format!("{name}\n{}", bounded_content(source, &extension));
        if contains_any(&searchable, &["invoice", "faktura", "receipt", "kvittering", "bank statement", "kontoudtog", "årsopgørelse", "tax statement"]) {
            return Ok(Some(root(node, "documentsRoot", "~/Documents")?.join("Finance")));
        }
        let has_cv_token = name.split(|character: char| !character.is_alphanumeric()).any(|part| part == "cv");
        if has_cv_token || contains_any(&searchable, &["curriculum vitae", "resume", "résumé", "job application", "jobansøgning", "cover letter", "ansøgning til"]) {
            return Ok(Some(root(node, "documentsRoot", "~/Documents")?.join("Career")));
        }
        if contains_any(&searchable, &["assignment", "lecture notes", "syllabus", "exam", "opgave", "forelæsning", "pensum", "eksamen"]) {
            return Ok(Some(root(node, "documentsRoot", "~/Documents")?.join("Study")));
        }
        if contains_any(&searchable, &["boarding pass", "flight booking", "hotel booking", "itinerary", "rejseplan", "flybillet", "hotelreservation"]) {
            return Ok(Some(root(node, "documentsRoot", "~/Documents")?.join("Travel")));
        }
    }

    let category = if name.contains("screenshot") || name.contains("skærmbillede") {
        ("picturesRoot", "~/Pictures", "Screenshots")
    } else if mime.as_deref().is_some_and(|value| value.starts_with("image/"))
        || matches!(extension.as_str(), "jpg" | "jpeg" | "png" | "gif" | "webp" | "heic" | "heif" | "svg" | "raw" | "dng" | "tif" | "tiff" | "bmp")
    {
        ("picturesRoot", "~/Pictures", "Images")
    } else if mime.as_deref().is_some_and(|value| value.starts_with("video/"))
        || matches!(extension.as_str(), "mp4" | "mkv" | "mov" | "webm" | "avi" | "m4v")
    {
        ("videosRoot", "~/Videos", "")
    } else if mime.as_deref().is_some_and(|value| value.starts_with("audio/"))
        || matches!(extension.as_str(), "mp3" | "flac" | "wav" | "m4a" | "ogg" | "opus" | "aac")
    {
        ("musicRoot", "~/Music", "")
    } else if mime.as_deref().is_some_and(|value| matches!(value, "application/zip" | "application/gzip" | "application/x-rar-compressed" | "application/x-7z-compressed"))
        || matches!(extension.as_str(), "zip" | "7z" | "rar" | "tar" | "gz" | "bz2" | "xz" | "zst")
    {
        ("archivesRoot", "~/Documents/Archives", "")
    } else if mime.as_deref() == Some("application/vnd.microsoft.portable-executable")
        || matches!(extension.as_str(), "rpm" | "deb" | "appimage" | "msi" | "exe" | "dmg" | "pkg" | "flatpakref")
    {
        ("softwareRoot", "~/Downloads/Software", "")
    } else if matches!(extension.as_str(), "xls" | "xlsx" | "ods" | "csv") {
        ("documentsRoot", "~/Documents", "Spreadsheets")
    } else if matches!(extension.as_str(), "ppt" | "pptx" | "odp") {
        ("documentsRoot", "~/Documents", "Presentations")
    } else if matches!(extension.as_str(), "epub" | "mobi" | "azw" | "azw3") {
        ("documentsRoot", "~/Documents", "Ebooks")
    } else if extension == "pdf" || mime.as_deref() == Some("application/pdf") {
        ("documentsRoot", "~/Documents", "PDFs")
    } else if matches!(extension.as_str(), "doc" | "docx" | "odt" | "rtf" | "txt" | "md") {
        ("documentsRoot", "~/Documents", "Text")
    } else {
        ("otherRoot", "~/Downloads/Other", "")
    };

    let mut directory = root(node, category.0, category.1)?;
    if !category.2.is_empty() {
        directory.push(category.2);
    }
    Ok(Some(directory))
}

fn root(node: &WorkflowNode, key: &str, fallback: &str) -> Result<PathBuf, String> {
    let raw = node
        .data
        .config
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(fallback);
    if raw == "~" {
        return dirs::home_dir().ok_or_else(|| "Home directory is unavailable".into());
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return dirs::home_dir()
            .map(|home| home.join(rest))
            .ok_or_else(|| "Home directory is unavailable".into());
    }
    Ok(PathBuf::from(raw))
}

fn contains_any(text: &str, terms: &[&str]) -> bool {
    let lowercase = text.to_lowercase();
    terms.iter().any(|term| lowercase.contains(term))
}

fn bounded_content(path: &Path, extension: &str) -> String {
    if extension == "pdf" {
        if let Ok(output) = Command::new("pdftotext")
            .args(["-f", "1", "-l", "3", "-nopgbrk"])
            .arg(path)
            .arg("-")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
        {
            if output.status.success() {
                return String::from_utf8_lossy(&output.stdout[..output.stdout.len().min(SAMPLE_LIMIT as usize)]).into_owned();
            }
        }
    }
    if matches!(extension, "docx" | "xlsx" | "pptx") {
        if let Ok(file) = fs::File::open(path) {
            if let Ok(mut archive) = ZipArchive::new(file) {
                let mut text = String::new();
                for index in 0..archive.len().min(200) {
                    let Ok(entry) = archive.by_index(index) else { continue };
                    let name = entry.name().to_owned();
                    let relevant = name == "word/document.xml"
                        || name == "xl/sharedStrings.xml"
                        || (name.starts_with("ppt/slides/slide") && name.ends_with(".xml"));
                    if relevant {
                        let remaining = SAMPLE_LIMIT.saturating_sub(text.len() as u64);
                        if remaining == 0 {
                            break;
                        }
                        let _ = entry.take(remaining).read_to_string(&mut text);
                    }
                }
                return text;
            }
        }
    }
    let Ok(file) = fs::File::open(path) else { return String::new() };
    let mut bytes = Vec::new();
    let _ = file.take(SAMPLE_LIMIT).read_to_end(&mut bytes);
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{NodeData, WorkflowNode};
    use serde_json::Value;
    use std::collections::HashMap;

    fn node(root: &Path) -> WorkflowNode {
        WorkflowNode {
            id: "organizer".into(),
            data: NodeData {
                label: "Organize".into(),
                kind: "action".into(),
                node_type: "organize_downloads".into(),
                config: HashMap::from([
                    ("documentsRoot".into(), Value::String(root.join("Documents").display().to_string())),
                    ("picturesRoot".into(), Value::String(root.join("Pictures").display().to_string())),
                ]),
            },
        }
    }

    #[test]
    fn routes_document_content_before_generic_type() {
        let root = std::env::temp_dir().join(format!("conduit-download-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("document.txt");
        fs::write(&source, "Invoice 4821 for services").unwrap();
        assert_eq!(destination(&source, &node(&root)).unwrap(), Some(root.join("Documents/Finance")));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn detects_image_content_with_an_unknown_extension() {
        let root = std::env::temp_dir().join(format!("conduit-image-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("download.bin");
        fs::write(&source, [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0]).unwrap();
        assert_eq!(destination(&source, &node(&root)).unwrap(), Some(root.join("Pictures/Images")));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ignores_partial_browser_downloads() {
        let root = std::env::temp_dir().join(format!("conduit-partial-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("video.mp4.part");
        fs::write(&source, b"partial").unwrap();
        assert_eq!(destination(&source, &node(&root)).unwrap(), None);
        fs::remove_dir_all(root).unwrap();
    }
}
