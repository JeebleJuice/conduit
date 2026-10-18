# Conduit

Conduit is a local-first Linux desktop app for building understandable automations from triggers, conditions, and actions on a visual canvas. It is based on the product concept in `Conduit - Visual Desktop Automation.md`.

This repository contains the first releasable slice:

- visual workflow editor with typed trigger, condition, and action nodes;
- connection validation and cycle prevention;
- editable node configuration and per-workflow folder permissions;
- dry-run plans with exact sources, destinations, warnings, and file limits;
- preview approval required again after a workflow changes;
- collision-safe copy and move actions with overwriting disabled;
- local SQLite run history in the Tauri app;
- browser-local fallback for UI development;
- workflow and history import/export.

The Rust engine currently executes create-directory, copy, move, text-writing, and checksum actions. Scheduling, live filesystem watching, background operation, trash/undo, archive handling, notifications, and controlled HTTP requests are the next engine milestones; their nodes are represented in the editor without pretending unsupported effects ran.

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

Conduit expands and resolves every filesystem path in the Rust process. A resolved source or destination must be within one of the workflow's explicit folder permissions. Filesystem runs are rejected unless the exact workflow definition has completed a dry run during the current app session. File discovery is bounded, and destination collisions receive a numeric suffix instead of overwriting data.

Run data lives in the platform application-data directory as `conduit.sqlite3`. Workflow drafts stay local in the webview's local storage.

## Checks

```bash
npm run build
cargo test --manifest-path src-tauri/Cargo.toml
```

The Rust check needs the native Fedora packages listed above.
