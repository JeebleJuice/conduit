use crate::models::WorkflowNode;
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use zip::ZipArchive;

const SAMPLE_LIMIT: u64 = 256 * 1024;
const ROM_ARCHIVE_LIMIT: u64 = 100 * 1024 * 1024 * 1024;

#[derive(Debug, PartialEq)]
pub(crate) enum Organization {
    Move(PathBuf),
    ExtractRom(PathBuf),
}

pub(crate) fn is_incomplete_download(source: &Path) -> bool {
    matches!(extension(source).as_str(), "part" | "crdownload" | "download" | "tmp")
        || source.metadata().map(|metadata| metadata.len() == 0).unwrap_or(true)
}

pub(crate) fn organization(source: &Path, node: &WorkflowNode) -> Result<Option<Organization>, String> {
    let extension = extension(source);
    if is_incomplete_download(source) {
        return Ok(None);
    }

    if extension == "pkg" {
        return Ok(Some(Organization::Move(root(node, "ps4Root", "~/Programs/PS4")?)));
    }
    if let Some(console) = direct_rom_console(source, &extension) {
        return Ok(Some(Organization::Move(
            root(node, "romsRoot", "~/Programs/Roms")?.join(console),
        )));
    }
    if matches!(extension.as_str(), "zip" | "7z") {
        if let Some(console) = rom_archive_console(source)? {
            return Ok(Some(Organization::ExtractRom(
                root(node, "romsRoot", "~/Programs/Roms")?.join(console),
            )));
        }
    }

    destination(source, node).map(|destination| destination.map(Organization::Move))
}

pub(crate) fn destination(source: &Path, node: &WorkflowNode) -> Result<Option<PathBuf>, String> {
    let extension = extension(source);
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
        ("softwareRoot", "~/Programs", "")
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
        return Ok(None);
    };

    let mut directory = root(node, category.0, category.1)?;
    if !category.2.is_empty() {
        directory.push(category.2);
    }
    Ok(Some(directory))
}

fn extension(path: &Path) -> String {
    path.extension()
        .map(|value| value.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

fn direct_rom_console(source: &Path, extension: &str) -> Option<&'static str> {
    match extension {
        "nes" => Some("NES"),
        "sfc" | "smc" => Some("SNES"),
        "n64" | "v64" | "z64" => Some("N64"),
        "gcm" => Some("GameCube"),
        "wbfs" | "wad" => Some("Wii"),
        "wux" | "wud" | "wua" | "rpx" => Some("Wii U"),
        "cue" | "chd" | "pbp" => Some("PS1"),
        "bin" if source.with_extension("cue").exists() => Some("PS1"),
        "iso" => disc_console(&file_prefix(source, 32)),
        _ => None,
    }
}

fn rom_archive_console(source: &Path) -> Result<Option<&'static str>, String> {
    let entries = archive_entries(source)?;
    let has_cue = entries.iter().any(|entry| extension(Path::new(entry)) == "cue");
    let has_wii_u_layout = ["code", "content", "meta"].iter().all(|expected| {
        entries.iter().any(|entry| {
            Path::new(entry).components().any(|component| {
                component.as_os_str().to_string_lossy().eq_ignore_ascii_case(expected)
            })
        })
    });
    if has_wii_u_layout {
        return Ok(Some("Wii U"));
    }
    for entry in &entries {
        let entry_extension = extension(Path::new(entry));
        let console = match entry_extension.as_str() {
            "nes" => Some("NES"),
            "sfc" | "smc" => Some("SNES"),
            "n64" | "v64" | "z64" => Some("N64"),
            "gcm" => Some("GameCube"),
            "wbfs" | "wad" => Some("Wii"),
            "wux" | "wud" | "wua" | "rpx" => Some("Wii U"),
            "cue" | "chd" | "pbp" => Some("PS1"),
            "bin" if has_cue => Some("PS1"),
            "iso" => disc_console(&archive_entry_prefix(source, entry, 32)),
            _ => None,
        };
        if console.is_some() {
            return Ok(console);
        }
    }
    Ok(None)
}

fn disc_console(prefix: &[u8]) -> Option<&'static str> {
    if prefix.get(0x1c..0x20) == Some(&0xc2339f3d_u32.to_be_bytes()) {
        Some("GameCube")
    } else if prefix.get(0x18..0x1c) == Some(&0x5d1c9ea3_u32.to_be_bytes()) {
        Some("Wii")
    } else {
        None
    }
}

fn file_prefix(path: &Path, limit: usize) -> Vec<u8> {
    let Ok(file) = fs::File::open(path) else { return Vec::new() };
    let mut bytes = Vec::new();
    let _ = file.take(limit as u64).read_to_end(&mut bytes);
    bytes
}

fn archive_entries(source: &Path) -> Result<Vec<String>, String> {
    if let Ok(file) = fs::File::open(source) {
        if let Ok(mut archive) = ZipArchive::new(file) {
            if archive.len() > 10_000 {
                return Err("ROM archive exceeded the 10,000 entry safety limit".into());
            }
            let mut entries = Vec::new();
            let mut total = 0_u64;
            for index in 0..archive.len() {
                let entry = archive.by_index(index).map_err(|error| error.to_string())?;
                total = total.saturating_add(entry.size());
                if total > ROM_ARCHIVE_LIMIT {
                    return Err("ROM archive exceeded the 100 GB extraction safety limit".into());
                }
                validate_relative_path(Path::new(entry.name()))?;
                entries.push(entry.name().to_owned());
            }
            return Ok(entries);
        }
    }

    let output = Command::new("7z")
        .args(["l", "-slt", "-ba", "--"])
        .arg(source)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("Cannot inspect {} with 7z: {error}", source.display()))?;
    if !output.status.success() {
        return Err(format!(
            "Cannot inspect {} as a completed archive: {}",
            source.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let listing = String::from_utf8_lossy(&output.stdout);
    let entries: Vec<String> = listing
        .lines()
        .filter_map(|line| line.strip_prefix("Path = ").map(str::to_owned))
        .collect();
    if entries.len() > 10_000 {
        return Err("ROM archive exceeded the 10,000 entry safety limit".into());
    }
    for entry in &entries {
        validate_relative_path(Path::new(entry))?;
    }
    let total = listing
        .lines()
        .filter_map(|line| line.strip_prefix("Size = "))
        .filter_map(|size| size.parse::<u64>().ok())
        .fold(0_u64, u64::saturating_add);
    if total > ROM_ARCHIVE_LIMIT {
        return Err("ROM archive exceeded the 100 GB extraction safety limit".into());
    }
    Ok(entries)
}

fn archive_entry_prefix(source: &Path, entry_name: &str, limit: usize) -> Vec<u8> {
    if let Ok(file) = fs::File::open(source) {
        if let Ok(mut archive) = ZipArchive::new(file) {
            if let Ok(entry) = archive.by_name(entry_name) {
                let mut bytes = Vec::new();
                let _ = entry.take(limit as u64).read_to_end(&mut bytes);
                return bytes;
            }
        }
    }
    let Ok(mut child) = Command::new("7z")
        .args(["x", "-so", "--"])
        .arg(source)
        .arg(entry_name)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return Vec::new();
    };
    let mut bytes = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        let _ = stdout.take(limit as u64).read_to_end(&mut bytes);
    }
    let _ = child.kill();
    let _ = child.wait();
    bytes
}

fn validate_relative_path(path: &Path) -> Result<(), String> {
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::RootDir | std::path::Component::Prefix(_)
            )
        })
    {
        Err("ROM archive contains an unsafe path".into())
    } else {
        Ok(())
    }
}

pub(crate) fn extract_rom_archive(source: &Path, destination: &Path) -> Result<(), String> {
    let _ = archive_entries(source)?;
    let parent = destination.parent().ok_or("ROM destination has no parent folder")?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    fs::create_dir_all(destination).map_err(|error| error.to_string())?;
    let staging = parent.join(format!(".conduit-rom-extract-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&staging).map_err(|error| error.to_string())?;

    let extraction = extract_archive_to(source, &staging).and_then(|_| {
        validate_extracted_tree(&staging)?;
        let entries = fs::read_dir(&staging).map_err(|error| error.to_string())?;
        for entry in entries {
            let entry = entry.map_err(|error| error.to_string())?;
            let target = collision_safe(destination.join(entry.file_name()));
            fs::rename(entry.path(), target).map_err(|error| error.to_string())?;
        }
        fs::remove_file(source).map_err(|error| format!("ROMs were extracted, but the archive could not be deleted: {error}"))
    });
    let _ = fs::remove_dir_all(&staging);
    extraction
}

fn extract_archive_to(source: &Path, destination: &Path) -> Result<(), String> {
    if let Ok(file) = fs::File::open(source) {
        if let Ok(mut archive) = ZipArchive::new(file) {
            for index in 0..archive.len() {
                let mut entry = archive.by_index(index).map_err(|error| error.to_string())?;
                let relative = entry.enclosed_name().ok_or("ROM archive contains an unsafe path")?;
                let output = destination.join(relative);
                if entry.is_dir() {
                    fs::create_dir_all(output).map_err(|error| error.to_string())?;
                } else {
                    if let Some(parent) = output.parent() {
                        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                    }
                    let mut file = fs::File::create(output).map_err(|error| error.to_string())?;
                    std::io::copy(&mut entry, &mut file).map_err(|error| error.to_string())?;
                }
            }
            return Ok(());
        }
    }
    let output = Command::new("7z")
        .arg("x")
        .arg("-y")
        .arg(format!("-o{}", destination.display()))
        .arg("--")
        .arg(source)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("Cannot extract {} with 7z: {error}", source.display()))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("7z extraction failed: {}", String::from_utf8_lossy(&output.stderr).trim()))
    }
}

fn validate_extracted_tree(root: &Path) -> Result<(), String> {
    let mut pending = vec![root.to_path_buf()];
    let mut count = 0_usize;
    let mut total = 0_u64;
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            let metadata = entry.path().symlink_metadata().map_err(|error| error.to_string())?;
            if metadata.file_type().is_symlink() {
                return Err("ROM archive contains a symbolic link".into());
            }
            count += 1;
            if count > 10_000 {
                return Err("ROM archive exceeded the 10,000 entry safety limit".into());
            }
            if metadata.is_dir() {
                pending.push(entry.path());
            } else {
                total = total.saturating_add(metadata.len());
                if total > ROM_ARCHIVE_LIMIT {
                    return Err("ROM archive exceeded the 100 GB extraction safety limit".into());
                }
            }
        }
    }
    Ok(())
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
    use std::io::Write;
    use zip::{write::SimpleFileOptions, ZipWriter};

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
                    ("romsRoot".into(), Value::String(root.join("Programs/Roms").display().to_string())),
                    ("ps4Root".into(), Value::String(root.join("Programs/PS4").display().to_string())),
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

    #[test]
    fn leaves_unrecognized_files_in_downloads() {
        let root = std::env::temp_dir().join(format!("conduit-unknown-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("mystery.unknown-format");
        fs::write(&source, b"unrecognized data").unwrap();
        assert_eq!(destination(&source, &node(&root)).unwrap(), None);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn routes_ps4_packages_to_the_ps4_folder() {
        let root = std::env::temp_dir().join(format!("conduit-pkg-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("game.pkg");
        fs::write(&source, b"package").unwrap();
        assert_eq!(
            organization(&source, &node(&root)).unwrap(),
            Some(Organization::Move(root.join("Programs/PS4")))
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn extracts_a_snes_archive_and_deletes_it_after_success() {
        let root = std::env::temp_dir().join(format!("conduit-rom-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("game.zip");
        let file = fs::File::create(&source).unwrap();
        let mut archive = ZipWriter::new(file);
        archive.start_file("Game/game.sfc", SimpleFileOptions::default()).unwrap();
        archive.write_all(b"rom").unwrap();
        archive.finish().unwrap();

        let destination = root.join("Programs/Roms/SNES");
        assert_eq!(
            organization(&source, &node(&root)).unwrap(),
            Some(Organization::ExtractRom(destination.clone()))
        );
        extract_rom_archive(&source, &destination).unwrap();
        assert!(destination.join("Game/game.sfc").is_file());
        assert!(!source.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recognizes_and_extracts_a_wii_u_wux_archive() {
        let root = std::env::temp_dir().join(format!("conduit-wii-u-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("Wind Waker HD.zip");
        let file = fs::File::create(&source).unwrap();
        let mut archive = ZipWriter::new(file);
        archive.start_file("Wind Waker HD.wux", SimpleFileOptions::default()).unwrap();
        archive.write_all(b"wii u disc image").unwrap();
        archive.finish().unwrap();

        let destination = root.join("Programs/Roms/Wii U");
        assert_eq!(
            organization(&source, &node(&root)).unwrap(),
            Some(Organization::ExtractRom(destination.clone()))
        );
        extract_rom_archive(&source, &destination).unwrap();
        assert!(destination.join("Wind Waker HD.wux").is_file());
        assert!(!source.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recognizes_a_wii_u_extracted_game_layout() {
        let root = std::env::temp_dir().join(format!("conduit-wii-u-layout-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("Wii U game.zip");
        let file = fs::File::create(&source).unwrap();
        let mut archive = ZipWriter::new(file);
        for entry in ["Game/code/game.rpx", "Game/content/data.bin", "Game/meta/meta.xml"] {
            archive.start_file(entry, SimpleFileOptions::default()).unwrap();
            archive.write_all(b"data").unwrap();
        }
        archive.finish().unwrap();

        assert_eq!(
            organization(&source, &node(&root)).unwrap(),
            Some(Organization::ExtractRom(root.join("Programs/Roms/Wii U")))
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn zero_byte_browser_placeholders_are_left_alone() {
        let root = std::env::temp_dir().join(format!("conduit-placeholder-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("pending.pkg");
        fs::write(&source, []).unwrap();
        assert_eq!(organization(&source, &node(&root)).unwrap(), None);
        fs::remove_dir_all(root).unwrap();
    }
}
