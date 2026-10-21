# Conduit threat model

## Assets and trust boundaries

Conduit can read and modify user-approved folders, issue network requests, call a small set of optional desktop tools, and retain execution metadata. Workflow definitions and imported files are untrusted input. The React interface is not a security boundary; every privileged decision is repeated by the Rust engine.

## Principal risks and controls

| Risk | Control |
| --- | --- |
| A workflow escapes an approved folder | Paths are expanded, canonicalised, and checked against exact permission roots in Rust. Destinations are checked even before they exist. |
| Symlink or traversal escapes | Existing paths are canonicalised. ZIP entries require `enclosed_name`, rejecting absolute and parent-traversal entries. |
| Accidental overwrite | Copy, move, rename, text, checksum, archive, and conversion outputs receive collision-safe names. |
| A changed workflow runs unattended | A SHA-256 signature covers nodes, edges, configuration, name, and folder permissions. Material changes revoke approval and disable the workflow. |
| A watcher triggers itself forever | Successful input and output fingerprints are recorded before queued watcher events are processed. Outputs are ignored unless they change again. |
| Duplicate native events | Per-workflow cooldowns and durable file fingerprints suppress duplicates. A reconciliation scan covers missed events. |
| Excessive work | File discovery is capped at 500, execution is serialized, retries are capped at three, HTTP responses are capped at 1 MB, archive creation is capped at 10,000 files/10 GB, extraction is capped at 10,000 entries/2 GB, and external commands have bounded arguments. |
| Shell injection | Standard nodes never invoke a shell. Optional tools are launched directly with argument arrays. Arbitrary command execution is absent. |
| Dangerous network access | HTTP permits HTTPS only, except loopback HTTP. Redirects are limited to three and requests time out after 15 seconds. |
| Secret disclosure | Bearer tokens live in the operating-system keyring and are never written to workflow JSON, SQLite, logs, or command arguments. |
| Webhook exposure | Webhooks bind only to `127.0.0.1`, accept POST on the configured path, and never listen on a LAN interface. |
| Crash ambiguity | Active runs are journaled before effects begin. A stale active run is recorded as failed during recovery; success is never inferred. |
| Misleading undo | Only journalled reversible operations expose Undo. Conflicts stop reversal rather than overwriting newer files. Removed copies go to desktop trash. |
| Malicious imported workflow | Imports start disabled and require explicit permissions plus a successful dry run. |
| Download misclassification | Content reads are bounded, partial-download suffixes are ignored, moves remain collision-safe and undoable, and the workflow starts disabled. |
| Unattended mail deletion | The KMail integration never deletes mail. Scheduled mode can only create a review report; applying that report is rejected unless the workflow trigger is manual, and the underlying tool revalidates IDs before moving mail. |
| Duplicate catch-up run | Each scheduled minute is claimed atomically in SQLite before execution, so restarts cannot replay the same slot. Catch-up is opt-in and capped at seven days. |

## Accepted limitations

- A local process running as the same user can modify Conduit’s SQLite database or workflow storage.
- Files can change between the last path check and the filesystem operation. Conduit narrows this window but cannot eliminate all local races without platform-specific sandboxing.
- HTTP requests and effects in external applications are not reversible.
- Optional tools (`ffmpeg`, `git`, `tesseract`, and `exiftool`) are trusted executables resolved from the user’s `PATH`.
- OS trash restoration remains the responsibility of the desktop environment.

## Release security checklist

1. Run frontend and Rust tests.
2. Run dependency audits.
3. Test path canonicalisation, ZIP traversal rejection, collision naming, approval revocation, and native watcher execution.
4. Build release artifacts only in the checked-in GitHub workflow or a controlled workstation.
5. Inspect draft artifacts before publishing and sign them when signing infrastructure is configured.
