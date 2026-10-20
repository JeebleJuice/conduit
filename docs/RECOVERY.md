# Recovery guide

## Stop all automation

Choose **Quit Conduit** from the tray menu. If the tray is unavailable, terminate the `conduit` process from the system monitor. The engine records any unfinished journal entry as interrupted on the next start.

## Disable one workflow

Open Conduit and switch the workflow off. If its definition changed after approval, Conduit already disabled it automatically.

## Review or undo a run

Open **History** and choose **Review** beside a reversible run. Conduit reverses actions in reverse order:

- moves and renames return to the original path only when that path is free;
- copies, generated text, checksums, media conversions, and ZIP archives go to desktop trash;
- created directories are removed only when still empty.

HTTP requests, notifications, OCR, metadata reads, extraction, Git reads, and trash actions are not automatically reversed.

## Database location and backup

The runtime database is `conduit.sqlite3` in the operating system’s application-data directory:

- Linux: normally `~/.local/share/dev.jeeblejuice.conduit/`
- macOS: normally `~/Library/Application Support/dev.jeeblejuice.conduit/`
- Windows: normally `%APPDATA%\dev.jeeblejuice.conduit\`

Quit Conduit before copying the database. SQLite WAL files belong to the same backup if the application was not stopped cleanly.

## Reset runtime state

1. Quit Conduit.
2. Move `conduit.sqlite3`, `conduit.sqlite3-wal`, and `conduit.sqlite3-shm` to a backup folder.
3. Start Conduit. A new database is created.
4. Every workflow returns disabled and must pass a dry run again.

Workflow drafts also exist in the webview’s local storage. Export important workflows from the interface before resetting application data.

## Optional tool failures

The engine status panel reports whether Git, FFmpeg, Tesseract, and ExifTool are available. Install the missing tool, restart Conduit, and run a new dry run. Conduit does not silently skip an unavailable executable.
