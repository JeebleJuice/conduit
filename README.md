# Conduit

Conduit is a local-first Linux desktop app for building understandable automations from triggers, conditions, and actions on a visual canvas. It is based on the product concept in `Conduit - Visual Desktop Automation.md`.

This repository contains the first releasable slice plus the Stage 3 background engine:

- visual workflow editor with typed trigger, condition, and action nodes;
- connection validation and cycle prevention;
- editable node configuration and per-workflow folder permissions;
- dry-run plans with exact sources, destinations, warnings, and file limits;
- preview approval required again after a workflow changes;
- collision-safe copy and move actions with overwriting disabled;
- local SQLite run history in the Tauri app;
- browser-local fallback for UI development;
- workflow and history import/export.
- durable enabled-workflow state and preview approvals;
- native folder watching with periodic reconciliation for missed events;
- five-field cron schedules and application-startup triggers;
- bounded retries, duplicate-file suppression, and single-run concurrency;
- crash recovery that records interrupted runs as failed rather than assuming success;
- a system-tray process that keeps enabled workflows alive after the window closes;
- live engine health, watcher counts, and runtime state in the interface.

The Rust engine currently executes create-directory, copy, move, rename, text-writing, and checksum actions. Trash/undo, archive handling, native notifications, and controlled HTTP requests remain later engine milestones; their nodes are represented in the editor without pretending unsupported effects ran.

## Run the interface

```bash
npm install
npm run dev
```

The browser version uses safe simulated runs, which makes UI work possible without filesystem access.

## Run the desktop app on Fedora

Install Tauri's native Linux prerequisites once:

```bash
sudo dnf install webkit2gtk4.1-devel libsoup3-devel
```

Then start Conduit:

```bash
npm run desktop
```

Build RPM and AppImage packages with:

```bash
npm run desktop:build
```

## Safety contract

Conduit expands and resolves every filesystem path in the Rust process. A resolved source or destination must be within one of the workflow's explicit folder permissions. Filesystem runs are rejected unless the exact workflow definition has completed a dry run. Approval is stored by a SHA-256 signature; any material edit revokes it and disables the workflow. File discovery is bounded, execution is serialized, retries are capped at three, and destination collisions receive a numeric suffix instead of overwriting data.

Run data, enabled workflow definitions, approvals, and processed-file fingerprints live in the platform application-data directory as `conduit.sqlite3`. Workflow drafts also stay local in the webview's local storage. Closing the window hides Conduit to the system tray; use **Quit Conduit** from the tray menu to stop the background engine.

Schedules use the standard five fields `minute hour day-of-month month weekday`. For example, `0 17 * * 5` runs Friday at 17:00 in the computer's local timezone.

## Checks

```bash
npm run build
cargo test --manifest-path src-tauri/Cargo.toml
```

The Rust check needs the native Fedora packages listed above.
