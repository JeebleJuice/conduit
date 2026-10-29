use crate::models::{NodeData, Workflow, WorkflowEdge, WorkflowNode};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

pub(crate) fn ps4usb_workflow() -> Workflow {
    Workflow {
        id: "ps4usb-library-sync".into(),
        name: "Sync ROM library to PS4USB".into(),
        enabled: true,
        permissions: vec![
            "~/Programs/Roms".into(),
            "~/Programs/PS4".into(),
            "/run/media/jeeblejuice/PS4USB".into(),
        ],
        nodes: vec![
            WorkflowNode {
                id: "ps4usb-trigger".into(),
                data: NodeData {
                    label: "PS4USB inserted".into(),
                    kind: "trigger".into(),
                    node_type: "drive_mounted".into(),
                    config: HashMap::from([
                        ("mountName".into(), serde_json::Value::String("PS4USB".into())),
                        ("mountUuid".into(), serde_json::Value::String("EAFF-37B0".into())),
                    ]),
                },
            },
            WorkflowNode {
                id: "ps4usb-sync".into(),
                data: NodeData {
                    label: "Update SSD library".into(),
                    kind: "action".into(),
                    node_type: "sync_removable_library".into(),
                    config: HashMap::from([
                        ("source".into(), serde_json::Value::String("~/Programs/Roms".into())),
                        ("packagesSource".into(), serde_json::Value::String("~/Programs/PS4".into())),
                        ("destination".into(), serde_json::Value::String("/run/media/jeeblejuice/PS4USB".into())),
                    ]),
                },
            },
            WorkflowNode {
                id: "ps4usb-notify".into(),
                data: NodeData {
                    label: "SSD ready".into(),
                    kind: "action".into(),
                    node_type: "notification".into(),
                    config: HashMap::from([(
                        "message".into(),
                        serde_json::Value::String(
                            "PS4USB sync finished. {input} It is safe to eject when no other program is using the drive.".into(),
                        ),
                    )]),
                },
            },
        ],
        edges: vec![
            WorkflowEdge { source: "ps4usb-trigger".into(), target: "ps4usb-sync".into() },
            WorkflowEdge { source: "ps4usb-sync".into(), target: "ps4usb-notify".into() },
        ],
    }
}

pub(crate) struct SyncSummary {
    pub copied_roms: usize,
    pub skipped_roms: usize,
    pub moved_packages: usize,
}

pub(crate) fn sync_library(
    roms_source: &Path,
    packages_source: &Path,
    destination_root: &Path,
    cancelled: &AtomicBool,
) -> Result<SyncSummary, String> {
    if !roms_source.is_dir() {
        return Err(format!("ROM source is unavailable: {}", roms_source.display()));
    }
    if !packages_source.is_dir() {
        return Err(format!("PS4 package source is unavailable: {}", packages_source.display()));
    }
    verify_writable(destination_root)?;

    let rom_files = recursive_files(roms_source)?;
    let mut packages: Vec<PathBuf> = fs::read_dir(packages_source)
        .map_err(|error| error.to_string())?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path.extension().is_some_and(|extension| extension.to_string_lossy().eq_ignore_ascii_case("pkg"))
        })
        .collect();
    packages.sort();

    let roms_destination = destination_root.join("Roms");
    let required = required_space(roms_source, &roms_destination, &rom_files)?
        .saturating_add(required_space(packages_source, destination_root, &packages)?);
    if let Some(available) = available_space(destination_root) {
        if required > available {
            return Err(format!(
                "The SSD needs {} more bytes for this sync ({} required, {} available)",
                required - available,
                required,
                available
            ));
        }
    }

    let mut summary = SyncSummary { copied_roms: 0, skipped_roms: 0, moved_packages: 0 };
    for source in rom_files {
        ensure_running(cancelled)?;
        let relative = source.strip_prefix(roms_source).map_err(|error| error.to_string())?;
        let destination = roms_destination.join(relative);
        if destination.exists() {
            ensure_matching_size(&source, &destination)?;
            summary.skipped_roms += 1;
            continue;
        }
        copy_atomic(&source, &destination, cancelled)?;
        summary.copied_roms += 1;
    }

    for source in packages {
        ensure_running(cancelled)?;
        let destination = destination_root.join(source.file_name().ok_or("A PS4 package has no filename")?);
        if destination.exists() {
            ensure_matching_size(&source, &destination)?;
            if digest(&source, cancelled)? != digest(&destination, cancelled)? {
                return Err(format!("Refusing to replace a different existing SSD package: {}", destination.display()));
            }
        } else {
            copy_atomic(&source, &destination, cancelled)?;
        }
        fs::remove_file(&source).map_err(|error| {
            format!("The package was copied to the SSD, but the local original could not be removed: {error}")
        })?;
        summary.moved_packages += 1;
    }
    Ok(summary)
}

fn recursive_files(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut directories = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(directory).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            let metadata = entry.path().symlink_metadata().map_err(|error| error.to_string())?;
            if metadata.file_type().is_symlink() {
                return Err(format!("Refusing to follow a ROM-library symlink: {}", entry.path().display()));
            }
            if metadata.is_dir() {
                directories.push(entry.path());
            } else if metadata.is_file() {
                files.push(entry.path());
            }
        }
    }
    files.sort();
    Ok(files)
}

fn required_space(source_root: &Path, destination_root: &Path, files: &[PathBuf]) -> Result<u64, String> {
    files.iter().try_fold(0_u64, |total, source| {
        let relative = source.strip_prefix(source_root).map_err(|error| error.to_string())?;
        let destination = destination_root.join(relative);
        if destination.exists() {
            ensure_matching_size(source, &destination)?;
            Ok(total)
        } else {
            Ok(total.saturating_add(source.metadata().map_err(|error| error.to_string())?.len()))
        }
    })
}

fn ensure_matching_size(source: &Path, destination: &Path) -> Result<(), String> {
    let source_size = source.metadata().map_err(|error| error.to_string())?.len();
    let destination_size = destination.metadata().map_err(|error| error.to_string())?.len();
    if source_size == destination_size {
        Ok(())
    } else {
        Err(format!(
            "Refusing to overwrite a different SSD file: {} (local {} bytes, SSD {} bytes)",
            destination.display(),
            source_size,
            destination_size
        ))
    }
}

fn copy_atomic(source: &Path, destination: &Path, cancelled: &AtomicBool) -> Result<(), String> {
    let parent = destination.parent().ok_or("A transfer destination has no parent folder")?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let temporary = parent.join(format!(
        ".{}.conduit-part-{}",
        destination.file_name().unwrap_or_default().to_string_lossy(),
        uuid::Uuid::new_v4()
    ));
    let result = (|| {
        let mut input = fs::File::open(source).map_err(|error| error.to_string())?;
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| error.to_string())?;
        let mut buffer = vec![0_u8; 8 * 1024 * 1024];
        let mut copied = 0_u64;
        loop {
            ensure_running(cancelled)?;
            let read = input.read(&mut buffer).map_err(|error| error.to_string())?;
            if read == 0 {
                break;
            }
            output.write_all(&buffer[..read]).map_err(|error| error.to_string())?;
            copied = copied.saturating_add(read as u64);
        }
        output.flush().map_err(|error| error.to_string())?;
        output.sync_all().map_err(|error| error.to_string())?;
        let expected = source.metadata().map_err(|error| error.to_string())?.len();
        if copied != expected {
            return Err(format!("Incomplete SSD copy for {}: copied {copied} of {expected} bytes", source.display()));
        }
        fs::rename(&temporary, destination).map_err(|error| error.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn digest(path: &Path, cancelled: &AtomicBool) -> Result<Vec<u8>, String> {
    let mut file = fs::File::open(path).map_err(|error| error.to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 8 * 1024 * 1024];
    loop {
        ensure_running(cancelled)?;
        let read = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().to_vec())
}

fn ensure_running(cancelled: &AtomicBool) -> Result<(), String> {
    if cancelled.load(Ordering::Relaxed) {
        Err("Run stopped by user".into())
    } else {
        Ok(())
    }
}

fn verify_writable(destination: &Path) -> Result<(), String> {
    if !destination.is_dir() {
        return Err(format!("SSD mount is unavailable: {}", destination.display()));
    }
    let probe = destination.join(format!(".conduit-write-probe-{}", uuid::Uuid::new_v4()));
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .map_err(|error| format!("SSD is not writable: {error}"))?;
    fs::remove_file(probe).map_err(|error| error.to_string())
}

#[cfg(unix)]
fn available_space(path: &Path) -> Option<u64> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return None;
    }
    let stats = unsafe { stats.assume_init() };
    Some(stats.f_bavail.saturating_mul(stats.f_frsize))
}

#[cfg(not(unix))]
fn available_space(_path: &Path) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_rom_tree_and_moves_only_packages() {
        let root = std::env::temp_dir().join(format!("conduit-removable-test-{}", uuid::Uuid::new_v4()));
        let roms = root.join("local/Roms");
        let packages = root.join("local/PS4");
        let drive = root.join("drive");
        fs::create_dir_all(roms.join("N64")).unwrap();
        fs::create_dir_all(&packages).unwrap();
        fs::create_dir_all(&drive).unwrap();
        fs::write(roms.join("N64/game.z64"), b"rom").unwrap();
        fs::write(packages.join("game.pkg"), b"package").unwrap();
        fs::write(packages.join("notes.txt"), b"keep").unwrap();

        let summary = sync_library(&roms, &packages, &drive, &AtomicBool::new(false)).unwrap();
        assert_eq!(summary.copied_roms, 1);
        assert_eq!(summary.moved_packages, 1);
        assert_eq!(fs::read(drive.join("Roms/N64/game.z64")).unwrap(), b"rom");
        assert_eq!(fs::read(drive.join("game.pkg")).unwrap(), b"package");
        assert!(roms.join("N64/game.z64").is_file());
        assert!(!packages.join("game.pkg").exists());
        assert!(packages.join("notes.txt").is_file());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn existing_rom_is_skipped_without_deleting_the_source() {
        let root = std::env::temp_dir().join(format!("conduit-removable-skip-test-{}", uuid::Uuid::new_v4()));
        let roms = root.join("local/Roms");
        let packages = root.join("local/PS4");
        let drive = root.join("drive");
        fs::create_dir_all(roms.join("SNES")).unwrap();
        fs::create_dir_all(&packages).unwrap();
        fs::create_dir_all(drive.join("Roms/SNES")).unwrap();
        fs::write(roms.join("SNES/game.sfc"), b"same").unwrap();
        fs::write(drive.join("Roms/SNES/game.sfc"), b"same").unwrap();
        let summary = sync_library(&roms, &packages, &drive, &AtomicBool::new(false)).unwrap();
        assert_eq!(summary.skipped_roms, 1);
        assert!(roms.join("SNES/game.sfc").is_file());
        fs::remove_dir_all(root).unwrap();
    }
}
