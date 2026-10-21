# Built-in desktop automations

Conduit adds new built-in workflows once per local profile. They start disabled. Open each workflow, check its paths, run **Dry run**, inspect the exact plan, then enable it. Editing a material setting revokes approval and disables the workflow again.

## Smart download organizer

The `Organize downloads` action watches only the top level of `~/Downloads`. Browser partial files ending in `.part`, `.crdownload`, `.download`, or `.tmp` are ignored.

Classification uses, in order:

1. bounded text from the filename and up to 256 KiB of readable content;
2. text from the first three PDF pages when `pdftotext` is available;
3. bounded OOXML text from DOCX, XLSX, and PPTX containers;
4. detected file signatures for common image, audio, video, and PDF data; and
5. the lowercase filename extension as a fallback.

Recognized finance, career, study, and travel documents go to matching subfolders under the configured Documents root. Other documents are grouped as PDFs, spreadsheets, presentations, ebooks, or text. Images and screenshots go under Pictures; audio and video use their XDG-style roots; archives use the configured archive folder; and installers and packages go to `~/Programs` by default. Files whose type remains unknown stay directly in Downloads.

Every resulting directory must remain inside the workflow permission roots. Directories are created when needed, existing files are never overwritten, and every move is journalled for Undo. Classification is intentionally conservative: content rules only select a broad destination and do not execute or upload the file.

## Weekly KMail cleanup preview

This workflow runs Sunday at 18:00 with a 24-hour catch-up window. If the computer was off at the exact time, Conduit claims the most recent missed slot once after it returns. Schedule claims live in SQLite, preventing duplicate execution after a restart.

The dedicated node can start the user-session Akonadi backend, then invokes only:

```text
~/Dev/kmail-cleanup/sort_inbox.py
```

Preview mode inspects the configured Inbox and writes a timestamped `candidates.csv` plus `summary.txt` under `~/Dev/kmail-cleanup/runs/`. It does not move or delete messages. Conduit then displays a reminder to review sender, subject, and classification reason.

## Apply reviewed KMail cleanup

After reviewing a `candidates.csv`, paste that exact path into the manual workflow's **Reviewed candidates.csv** field, dry-run the workflow, and run it. Conduit invokes the same fixed script with `--apply --report <exact path>`. The report must resolve inside the cleanup project's `runs/` directory. The Rust engine refuses apply mode unless the workflow trigger is manual—even if imported JSON tries to schedule it.

The existing KMail tool validates the account and folder IDs, verifies each message's current Akonadi ID and IMAP UID, moves only the reviewed rows in acknowledged batches, and stops on synchronization drift or timeout. Messages go to the visible `Cleanup Review` folder; they are not permanently deleted. This external mail operation is deliberately not advertised as undoable by Conduit.

## Weekly download review

Monday at 09:00, with a 12-hour catch-up window, this workflow scans the top level of `~/Downloads`, selects files older than seven days, and writes a Markdown inventory containing path, size, and modification time. It does not delete or move the listed files. The discovery limit is 100 files per run.

## Screenshot filing

The importable screenshot template watches `~/Pictures/Screenshots`, filters PNG files, and moves them into `{year}/{month}` subfolders. The date placeholders use local time, destinations are collision-safe, and the moves are undoable from History.

## Why these defaults

- XDG-style Documents, Downloads, Pictures, Music, and Videos roots keep output predictable on a Linux desktop.
- Scheduled maintenance creates review artifacts instead of deleting data.
- KMail's native filters remain the better choice for simple trusted sender rules; Conduit integrates the existing conservative classifier because it adds an explicit report/review/apply boundary.
- Missed schedules use bounded persistent catch-up instead of silently disappearing or replaying indefinitely.
- Cleanup actions use desktop trash or recoverable review folders rather than permanent deletion.

## Template files

- `smart-download-organizer.conduit.json`
- `weekly-kmail-cleanup-preview.conduit.json`
- `apply-reviewed-kmail-cleanup.conduit.json`
- `weekly-download-review.conduit.json`
- `monthly-screenshot-filing.conduit.json`

Imported copies receive a new workflow ID, remain disabled, and require their own preview approval.
