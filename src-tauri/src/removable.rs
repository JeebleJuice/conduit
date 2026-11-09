use crate::models::{NodeData, Workflow, WorkflowEdge, WorkflowNode};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
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
            "~/Programs/Homebrew Covers".into(),
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
                        (
                            "coversSource".into(),
                            serde_json::Value::String("~/Programs/Homebrew Covers".into()),
                        ),
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
    pub updated_rom_items: usize,
    pub unchanged_rom_items: usize,
    pub copied_rom_files: usize,
    pub relocated_rom_files: usize,
    pub skipped_rom_files: usize,
    pub copied_cover_files: usize,
    pub moved_packages: usize,
}

pub(crate) struct TransferPlan {
    pub bytes: u64,
    pub rom_files: usize,
    pub relocated_rom_files: usize,
    pub cover_files: usize,
    pub packages: usize,
}

struct RomTransferPlan {
    bytes: u64,
    copies: usize,
    relocations: usize,
}

pub(crate) fn sync_library(
    roms_source: &Path,
    packages_source: &Path,
    covers_source: Option<&Path>,
    destination_root: &Path,
    cancelled: &AtomicBool,
    on_transfer_started: Option<&dyn Fn(&TransferPlan)>,
) -> Result<SyncSummary, String> {
    if !roms_source.is_dir() {
        return Err(format!("ROM source is unavailable: {}", roms_source.display()));
    }
    if !packages_source.is_dir() {
        return Err(format!("PS4 package source is unavailable: {}", packages_source.display()));
    }
    if let Some(source) = covers_source.filter(|source| !source.is_dir()) {
        return Err(format!("Homebrew cover source is unavailable: {}", source.display()));
    }
    verify_writable(destination_root)?;

    let rom_files = recursive_files(roms_source)?;
    let cover_files = match covers_source {
        Some(source) => recursive_files(source)?,
        None => Vec::new(),
    };
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
    let covers_destination = destination_root.join("Homebrew Covers");
    let rom_plan = required_rom_space(roms_source, &roms_destination, &rom_files, cancelled)?;
    let (cover_bytes, cover_copies) = required_changed_space(
        covers_source,
        &covers_destination,
        &cover_files,
        cancelled,
    )?;
    let package_bytes = required_space(packages_source, destination_root, &packages)?;
    let required = rom_plan.bytes.saturating_add(cover_bytes).saturating_add(package_bytes);
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
    if rom_plan.copies + rom_plan.relocations + cover_copies + packages.len() > 0 {
        if let Some(notify) = on_transfer_started {
            notify(&TransferPlan {
                bytes: required,
                rom_files: rom_plan.copies,
                relocated_rom_files: rom_plan.relocations,
                cover_files: cover_copies,
                packages: packages.len(),
            });
        }
    }

    let mut summary = SyncSummary {
        updated_rom_items: 0,
        unchanged_rom_items: 0,
        copied_rom_files: 0,
        relocated_rom_files: 0,
        skipped_rom_files: 0,
        copied_cover_files: 0,
        moved_packages: 0,
    };
    let mut all_rom_items = HashSet::new();
    let mut updated_rom_items = HashSet::new();
    for source in rom_files {
        ensure_running(cancelled)?;
        let relative = source.strip_prefix(roms_source).map_err(|error| error.to_string())?;
        let item = rom_item(relative);
        all_rom_items.insert(item.clone());
        let destination = roms_destination.join(ssd_rom_relative(relative));
        if destination.exists() {
            if rom_file_is_current(&source, &destination, cancelled)? {
                summary.skipped_rom_files += 1;
                continue;
            }
            copy_atomic(&source, &destination, cancelled)?;
            summary.copied_rom_files += 1;
            updated_rom_items.insert(item);
            continue;
        }
        if let Some(legacy) = legacy_ssd_rom_path(&roms_destination, relative).filter(|path| path.exists()) {
            ensure_matching_size(&source, &legacy)?;
            let parent = destination.parent().ok_or("A Dolphin-library destination has no parent folder")?;
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            fs::rename(&legacy, &destination).map_err(|error| {
                format!("Could not reorganize {} into the SSD Dolphin library: {error}", legacy.display())
            })?;
            remove_empty_parents(legacy.parent(), &roms_destination);
            summary.relocated_rom_files += 1;
        } else {
            copy_atomic(&source, &destination, cancelled)?;
            summary.copied_rom_files += 1;
        }
        updated_rom_items.insert(item);
    }
    summary.updated_rom_items = updated_rom_items.len();
    summary.unchanged_rom_items = all_rom_items.difference(&updated_rom_items).count();

    if let Some(source_root) = covers_source {
        for source in cover_files {
            ensure_running(cancelled)?;
            let relative = source.strip_prefix(source_root).map_err(|error| error.to_string())?;
            let destination = covers_destination.join(relative);
            if destination.exists() && files_match(&source, &destination, cancelled)? {
                continue;
            }
            copy_atomic(&source, &destination, cancelled)?;
            summary.copied_cover_files += 1;
        }
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

fn ssd_rom_relative(relative: &Path) -> PathBuf {
    let mut components = relative.components();
    let Some(console) = components.next() else { return relative.to_path_buf() };
    let console_name = console.as_os_str().to_string_lossy();
    if console_name.eq_ignore_ascii_case("Wii") || console_name.eq_ignore_ascii_case("GameCube") {
        let mut mapped = PathBuf::from("Dolphin");
        mapped.extend(components.map(|component| component.as_os_str()));
        mapped
    } else {
        relative.to_path_buf()
    }
}

fn legacy_ssd_rom_path(roms_destination: &Path, relative: &Path) -> Option<PathBuf> {
    (ssd_rom_relative(relative) != relative).then(|| roms_destination.join(relative))
}

fn remove_empty_parents(mut current: Option<&Path>, stop: &Path) {
    while let Some(directory) = current {
        if directory == stop || fs::remove_dir(directory).is_err() {
            break;
        }
        current = directory.parent();
    }
}

fn rom_item(relative: &Path) -> PathBuf {
    let mut components = relative.components();
    let Some(console) = components.next() else { return relative.to_path_buf() };
    let mut item = PathBuf::from(console.as_os_str());
    if let Some(game) = components.next() {
        item.push(game.as_os_str());
    }
    item
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

const SMALL_ROM_FILE_LIMIT: u64 = 16 * 1024 * 1024;

fn required_rom_space(
    source_root: &Path,
    destination_root: &Path,
    files: &[PathBuf],
    cancelled: &AtomicBool,
) -> Result<RomTransferPlan, String> {
    let mut plan = RomTransferPlan { bytes: 0, copies: 0, relocations: 0 };
    let mut destinations = HashMap::new();
    for source in files {
        let relative = source.strip_prefix(source_root).map_err(|error| error.to_string())?;
        let mapped = ssd_rom_relative(relative);
        if let Some(previous) = destinations.insert(mapped.clone(), relative.to_path_buf()) {
            if previous != relative {
                return Err(format!(
                    "Wii and GameCube files would collide in the SSD Dolphin library: {} and {}",
                    previous.display(),
                    relative.display()
                ));
            }
        }
        let destination = destination_root.join(&mapped);
        if destination.exists() {
            if !rom_file_is_current(source, &destination, cancelled)? {
                plan.bytes = plan.bytes.saturating_add(source.metadata().map_err(|error| error.to_string())?.len());
                plan.copies += 1;
            }
        } else if let Some(legacy) = legacy_ssd_rom_path(destination_root, relative).filter(|path| path.exists()) {
            ensure_matching_size(source, &legacy)?;
            plan.relocations += 1;
        } else {
            plan.bytes = plan.bytes.saturating_add(source.metadata().map_err(|error| error.to_string())?.len());
            plan.copies += 1;
        }
    }
    Ok(plan)
}

fn required_changed_space(
    source_root: Option<&Path>,
    destination_root: &Path,
    files: &[PathBuf],
    cancelled: &AtomicBool,
) -> Result<(u64, usize), String> {
    let Some(source_root) = source_root else { return Ok((0, 0)) };
    let mut bytes = 0_u64;
    let mut copies = 0_usize;
    for source in files {
        let relative = source.strip_prefix(source_root).map_err(|error| error.to_string())?;
        let destination = destination_root.join(relative);
        if !destination.exists() || !files_match(source, &destination, cancelled)? {
            bytes = bytes.saturating_add(source.metadata().map_err(|error| error.to_string())?.len());
            copies += 1;
        }
    }
    Ok((bytes, copies))
}

fn rom_file_is_current(source: &Path, destination: &Path, cancelled: &AtomicBool) -> Result<bool, String> {
    let source_size = source.metadata().map_err(|error| error.to_string())?.len();
    let destination_size = destination.metadata().map_err(|error| error.to_string())?.len();
    if source_size != destination_size {
        return Ok(false);
    }
    if source_size <= SMALL_ROM_FILE_LIMIT {
        files_match(source, destination, cancelled)
    } else {
        Ok(true)
    }
}

fn files_match(source: &Path, destination: &Path, cancelled: &AtomicBool) -> Result<bool, String> {
    let source_size = source.metadata().map_err(|error| error.to_string())?.len();
    let destination_size = destination.metadata().map_err(|error| error.to_string())?.len();
    Ok(source_size == destination_size && digest(source, cancelled)? == digest(destination, cancelled)?)
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

pub(crate) fn is_insufficient_space_error(error: &str) -> bool {
    insufficient_space_needed(error).is_some()
}

pub(crate) fn insufficient_space_needed(error: &str) -> Option<u64> {
    error
        .strip_prefix("The SSD needs ")?
        .split_once(" more bytes for this sync")?
        .0
        .parse()
        .ok()
}

#[cfg(unix)]
pub(crate) fn available_space(path: &Path) -> Option<u64> {
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
pub(crate) fn available_space(_path: &Path) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

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

        let announced = AtomicU64::new(0);
        let announce = |plan: &TransferPlan| announced.store(plan.bytes, Ordering::Relaxed);
        let summary = sync_library(
            &roms,
            &packages,
            None,
            &drive,
            &AtomicBool::new(false),
            Some(&announce),
        )
        .unwrap();
        assert_eq!(announced.load(Ordering::Relaxed), 10);
        assert_eq!(summary.updated_rom_items, 1);
        assert_eq!(summary.copied_rom_files, 1);
        assert_eq!(summary.moved_packages, 1);
        assert_eq!(fs::read(drive.join("Roms/N64/game.z64")).unwrap(), b"rom");
        assert_eq!(fs::read(drive.join("game.pkg")).unwrap(), b"package");
        assert!(roms.join("N64/game.z64").is_file());
        assert!(!packages.join("game.pkg").exists());
        assert!(packages.join("notes.txt").is_file());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn combines_wii_and_gamecube_files_in_the_ssd_dolphin_folder() {
        let root = std::env::temp_dir().join(format!("conduit-dolphin-test-{}", uuid::Uuid::new_v4()));
        let roms = root.join("local/Roms");
        let packages = root.join("local/PS4");
        let drive = root.join("drive");
        fs::create_dir_all(roms.join("Wii")).unwrap();
        fs::create_dir_all(roms.join("GameCube")).unwrap();
        fs::create_dir_all(&packages).unwrap();
        fs::create_dir_all(&drive).unwrap();
        fs::write(roms.join("Wii/Galaxy.nkit.iso"), b"wii").unwrap();
        fs::write(roms.join("GameCube/Sunshine.nkit.iso"), b"gamecube").unwrap();

        let summary = sync_library(&roms, &packages, None, &drive, &AtomicBool::new(false), None).unwrap();
        assert_eq!(summary.copied_rom_files, 2);
        assert_eq!(fs::read(drive.join("Roms/Dolphin/Galaxy.nkit.iso")).unwrap(), b"wii");
        assert_eq!(fs::read(drive.join("Roms/Dolphin/Sunshine.nkit.iso")).unwrap(), b"gamecube");
        assert!(!drive.join("Roms/Wii").exists());
        assert!(!drive.join("Roms/GameCube").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn relocates_existing_ssd_dolphin_games_without_recopying_them() {
        let root = std::env::temp_dir().join(format!("conduit-dolphin-migrate-test-{}", uuid::Uuid::new_v4()));
        let roms = root.join("local/Roms");
        let packages = root.join("local/PS4");
        let drive = root.join("drive");
        fs::create_dir_all(roms.join("Wii")).unwrap();
        fs::create_dir_all(&packages).unwrap();
        fs::create_dir_all(drive.join("Roms/Wii")).unwrap();
        fs::write(roms.join("Wii/Galaxy.nkit.iso"), b"wii").unwrap();
        fs::write(drive.join("Roms/Wii/Galaxy.nkit.iso"), b"wii").unwrap();

        let announced = AtomicBool::new(false);
        let announce = |plan: &TransferPlan| {
            assert_eq!(plan.bytes, 0);
            assert_eq!(plan.rom_files, 0);
            assert_eq!(plan.relocated_rom_files, 1);
            announced.store(true, Ordering::Relaxed);
        };
        let summary = sync_library(
            &roms,
            &packages,
            None,
            &drive,
            &AtomicBool::new(false),
            Some(&announce),
        )
        .unwrap();

        assert!(announced.load(Ordering::Relaxed));
        assert_eq!(summary.relocated_rom_files, 1);
        assert!(drive.join("Roms/Dolphin/Galaxy.nkit.iso").is_file());
        assert!(!drive.join("Roms/Wii").exists());
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
        let summary = sync_library(&roms, &packages, None, &drive, &AtomicBool::new(false), None).unwrap();
        assert_eq!(summary.unchanged_rom_items, 1);
        assert_eq!(summary.skipped_rom_files, 1);
        assert!(roms.join("SNES/game.sfc").is_file());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn groups_directory_format_rom_files_as_one_game() {
        let root = std::env::temp_dir().join(format!("conduit-removable-group-test-{}", uuid::Uuid::new_v4()));
        let roms = root.join("local/Roms");
        let packages = root.join("local/PS4");
        let drive = root.join("drive");
        let game = roms.join("Wii U/Twilight Princess");
        fs::create_dir_all(game.join("code")).unwrap();
        fs::create_dir_all(game.join("content")).unwrap();
        fs::create_dir_all(game.join("meta")).unwrap();
        fs::create_dir_all(&packages).unwrap();
        fs::create_dir_all(&drive).unwrap();
        fs::write(game.join("code/game.rpx"), b"code").unwrap();
        fs::write(game.join("content/data.app"), b"content").unwrap();
        fs::write(game.join("meta/meta.xml"), b"meta").unwrap();

        let summary = sync_library(&roms, &packages, None, &drive, &AtomicBool::new(false), None).unwrap();
        assert_eq!(summary.updated_rom_items, 1);
        assert_eq!(summary.copied_rom_files, 3);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stages_homebrew_covers_outside_the_rom_scanner() {
        let root = std::env::temp_dir().join(format!("conduit-cover-test-{}", uuid::Uuid::new_v4()));
        let roms = root.join("local/Roms");
        let packages = root.join("local/PS4");
        let covers = root.join("local/Homebrew Covers");
        let drive = root.join("drive");
        fs::create_dir_all(&roms).unwrap();
        fs::create_dir_all(&packages).unwrap();
        fs::create_dir_all(covers.join("DolphinPS4")).unwrap();
        fs::create_dir_all(&drive).unwrap();
        fs::write(covers.join("DolphinPS4/GMSE01.png"), b"cover").unwrap();

        let summary = sync_library(
            &roms,
            &packages,
            Some(&covers),
            &drive,
            &AtomicBool::new(false),
            None,
        )
        .unwrap();

        assert_eq!(summary.copied_cover_files, 1);
        assert_eq!(
            fs::read(drive.join("Homebrew Covers/DolphinPS4/GMSE01.png")).unwrap(),
            b"cover"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn refreshes_changed_small_rom_metadata_with_the_same_size() {
        let root = std::env::temp_dir().join(format!("conduit-metadata-test-{}", uuid::Uuid::new_v4()));
        let roms = root.join("local/Roms");
        let packages = root.join("local/PS4");
        let drive = root.join("drive");
        fs::create_dir_all(roms.join("Wii U/Twilight/meta")).unwrap();
        fs::create_dir_all(&packages).unwrap();
        fs::create_dir_all(drive.join("Roms/Wii U/Twilight/meta")).unwrap();
        fs::write(roms.join("Wii U/Twilight/meta/meta.xml"), b"base00").unwrap();
        fs::write(drive.join("Roms/Wii U/Twilight/meta/meta.xml"), b"update").unwrap();

        let summary = sync_library(&roms, &packages, None, &drive, &AtomicBool::new(false), None).unwrap();

        assert_eq!(summary.updated_rom_items, 1);
        assert_eq!(summary.copied_rom_files, 1);
        assert_eq!(fs::read(drive.join("Roms/Wii U/Twilight/meta/meta.xml")).unwrap(), b"base00");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn parses_the_insufficient_space_shortfall_for_notifications() {
        assert_eq!(
            insufficient_space_needed(
                "The SSD needs 12961617162 more bytes for this sync (73056032010 required, 60094414848 available)"
            ),
            Some(12_961_617_162)
        );
    }
}
