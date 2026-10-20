# Release checklist

- [ ] Update the version in `package.json`, `src-tauri/Cargo.toml`, and `src-tauri/tauri.conf.json`.
- [ ] Run `npm ci`, `npm test`, and `npm run build`.
- [ ] Run `cargo test --manifest-path src-tauri/Cargo.toml`.
- [ ] Run the large-folder, watcher, ZIP traversal, and approval-revocation tests.
- [ ] Test a dry run and a real run in disposable directories.
- [ ] Test close-to-tray, restart recovery, notification permission, webhook binding, and Undo.
- [ ] Review `docs/THREAT_MODEL.md` and `docs/RECOVERY.md`.
- [ ] Tag `vX.Y.Z`; let GitHub Actions create draft artifacts for Linux, Windows, and macOS.
- [ ] Smoke-test each artifact before publishing the draft release.
