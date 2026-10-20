# Cross-platform notes

Conduit’s engine uses portable Rust filesystem, HTTP, ZIP, keyring, trash, watcher, and tray APIs. Linux remains the reference platform; the release workflow also builds Windows NSIS and macOS DMG artifacts.

## Linux

Install WebKitGTK 4.1 and app-indicator development packages. Removable drives are detected under `/media` and `/run/media/$USER`. RPM and AppImage bundles are produced.

## Windows

The WebView2 runtime is required by Tauri. Removable volumes from `D:` through `Z:` are monitored. Secrets use Windows Credential Manager, trash uses the Recycle Bin, and releases use NSIS.

## macOS

Xcode command-line tools are required. Mounted volumes under `/Volumes` are monitored. Secrets use Keychain, trash uses Finder Trash, and releases produce `.app` and `.dmg` bundles.

Optional executable nodes require their corresponding command on `PATH` on every platform. Paths and workflow permissions should be changed after importing a workflow created on another operating system, followed by a new dry run.
