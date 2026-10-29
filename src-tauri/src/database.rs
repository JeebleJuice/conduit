use crate::engine::workflow_signature;
use crate::models::{ReversibleOperation, RunRecord, Workflow, WorkflowRuntimeStatus};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

pub(crate) fn open(path: &Path) -> Result<Connection, String> {
    let connection = Connection::open(path).map_err(|error| error.to_string())?;
    connection
        .pragma_update(None, "journal_mode", "WAL")
        .map_err(|error| error.to_string())?;
    connection
        .busy_timeout(std::time::Duration::from_secs(3))
        .map_err(|error| error.to_string())?;
    Ok(connection)
}

pub(crate) fn initialize(path: &Path) -> Result<(), String> {
    let connection = open(path)?;
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS runs (
                id TEXT PRIMARY KEY,
                workflow_id TEXT NOT NULL,
                workflow_name TEXT NOT NULL,
                started_at TEXT NOT NULL,
                duration_ms INTEGER NOT NULL,
                status TEXT NOT NULL,
                files INTEGER NOT NULL,
                reversible INTEGER NOT NULL,
                message TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS workflows (
                id TEXT PRIMARY KEY,
                definition TEXT NOT NULL,
                signature TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 0,
                approved_signature TEXT,
                runtime_state TEXT NOT NULL DEFAULT 'paused',
                last_error TEXT,
                last_run_at TEXT,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS active_runs (
                id TEXT PRIMARY KEY,
                workflow_id TEXT NOT NULL,
                workflow_name TEXT NOT NULL,
                started_at TEXT NOT NULL,
                trigger_kind TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS processed_files (
                workflow_id TEXT NOT NULL,
                path TEXT NOT NULL,
                modified_ns INTEGER NOT NULL,
                size INTEGER NOT NULL,
                processed_at TEXT NOT NULL,
                PRIMARY KEY (workflow_id, path, modified_ns, size)
            );
            CREATE TABLE IF NOT EXISTS run_operations (
                run_id TEXT NOT NULL,
                sequence INTEGER NOT NULL,
                action TEXT NOT NULL,
                source TEXT,
                destination TEXT,
                undone INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (run_id, sequence)
            );
            CREATE TABLE IF NOT EXISTS schedule_firings (
                workflow_id TEXT NOT NULL,
                scheduled_minute TEXT NOT NULL,
                claimed_at TEXT NOT NULL,
                PRIMARY KEY (workflow_id, scheduled_minute)
            );
            CREATE INDEX IF NOT EXISTS runs_workflow_started
                ON runs(workflow_id, started_at DESC);",
        )
        .map_err(|error| error.to_string())?;
    recover_interrupted_runs(&connection)
}

pub(crate) fn claim_schedule(path: &Path, workflow_id: &str, scheduled_minute: &str) -> Result<bool, String> {
    let connection = open(path)?;
    let inserted = connection
        .execute(
            "INSERT OR IGNORE INTO schedule_firings (workflow_id,scheduled_minute,claimed_at) VALUES (?1,?2,?3)",
            params![workflow_id, scheduled_minute, Utc::now().to_rfc3339()],
        )
        .map_err(|error| error.to_string())?;
    let _ = connection.execute(
        "DELETE FROM schedule_firings WHERE julianday(claimed_at) < julianday('now', '-35 days')",
        [],
    );
    Ok(inserted == 1)
}

fn recover_interrupted_runs(connection: &Connection) -> Result<(), String> {
    let mut statement = connection
        .prepare("SELECT id, workflow_id, workflow_name, started_at FROM active_runs")
        .map_err(|error| error.to_string())?;
    let interrupted = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;

    for (id, workflow_id, workflow_name, started_at) in interrupted {
        connection
            .execute(
                "INSERT OR REPLACE INTO runs VALUES (?1,?2,?3,?4,0,'failed',0,0,?5)",
                params![
                    id,
                    workflow_id,
                    workflow_name,
                    started_at,
                    "Interrupted by an app shutdown; no success was assumed"
                ],
            )
            .map_err(|error| error.to_string())?;
    }
    connection
        .execute("DELETE FROM active_runs", [])
        .map_err(|error| error.to_string())?;
    Ok(())
}

pub(crate) fn sync_workflow(path: &Path, workflow: &Workflow) -> Result<WorkflowRuntimeStatus, String> {
    let connection = open(path)?;
    let signature = workflow_signature(workflow)?;
    let definition = serde_json::to_string(workflow).map_err(|error| error.to_string())?;
    let previous: Option<(String, Option<String>, bool)> = connection
        .query_row(
            "SELECT signature, approved_signature, enabled FROM workflows WHERE id=?1",
            [&workflow.id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(|error| error.to_string())?;
    let changed = previous.as_ref().is_some_and(|(old, _, _)| old != &signature);
    let approved = previous
        .as_ref()
        .and_then(|(_, approved, _)| approved.as_ref())
        .is_some_and(|approved| approved == &signature);
    let enabled = previous.as_ref().map(|(_, _, enabled)| *enabled).unwrap_or(false) && !changed;
    let state = if changed {
        "needs_preview"
    } else if enabled {
        trigger_state(workflow)
    } else if approved {
        "ready"
    } else {
        "needs_preview"
    };
    connection
        .execute(
            "INSERT INTO workflows (id,definition,signature,enabled,approved_signature,runtime_state,last_error,updated_at)
             VALUES (?1,?2,?3,?4,NULL,?5,NULL,?6)
             ON CONFLICT(id) DO UPDATE SET
                definition=excluded.definition,
                signature=excluded.signature,
                enabled=CASE WHEN workflows.signature=excluded.signature THEN workflows.enabled ELSE 0 END,
                approved_signature=CASE WHEN workflows.signature=excluded.signature THEN workflows.approved_signature ELSE NULL END,
                runtime_state=excluded.runtime_state,
                last_error=NULL,
                updated_at=excluded.updated_at",
            params![workflow.id, definition, signature, enabled, state, Utc::now().to_rfc3339()],
        )
        .map_err(|error| error.to_string())?;
    status_for(&connection, &workflow.id)
}

pub(crate) fn workflow_exists(path: &Path, workflow_id: &str) -> Result<bool, String> {
    let connection = open(path)?;
    connection
        .query_row("SELECT EXISTS(SELECT 1 FROM workflows WHERE id=?1)", [workflow_id], |row| row.get(0))
        .map_err(|error| error.to_string())
}

pub(crate) fn approve_preview(path: &Path, workflow: &Workflow) -> Result<(), String> {
    sync_workflow(path, workflow)?;
    let connection = open(path)?;
    let signature = workflow_signature(workflow)?;
    connection
        .execute(
            "UPDATE workflows SET approved_signature=?2, runtime_state=CASE WHEN enabled=1 THEN runtime_state ELSE 'ready' END, last_error=NULL WHERE id=?1",
            params![workflow.id, signature],
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

pub(crate) fn is_approved(path: &Path, workflow: &Workflow) -> Result<bool, String> {
    let connection = open(path)?;
    let signature = workflow_signature(workflow)?;
    connection
        .query_row(
            "SELECT COALESCE(approved_signature=?2, 0) FROM workflows WHERE id=?1",
            params![workflow.id, signature],
            |row| row.get(0),
        )
        .optional()
        .map(|value| value.unwrap_or(false))
        .map_err(|error| error.to_string())
}

pub(crate) fn set_enabled(path: &Path, workflow: &Workflow, enabled: bool) -> Result<WorkflowRuntimeStatus, String> {
    sync_workflow(path, workflow)?;
    let connection = open(path)?;
    if enabled && !is_approved(path, workflow)? {
        return Err("Run a successful dry run before enabling this workflow".into());
    }
    let state = if enabled { trigger_state(workflow) } else { "paused" };
    connection
        .execute(
            "UPDATE workflows SET enabled=?2,runtime_state=?3,last_error=NULL,updated_at=?4 WHERE id=?1",
            params![workflow.id, enabled, state, Utc::now().to_rfc3339()],
        )
        .map_err(|error| error.to_string())?;
    status_for(&connection, &workflow.id)
}

fn trigger_state(workflow: &Workflow) -> &'static str {
    match workflow.nodes.iter().find(|node| node.data.kind == "trigger").map(|node| node.data.node_type.as_str()) {
        Some("folder_changed") => "watching",
        Some("schedule") => "scheduled",
        Some("startup") => "armed",
        Some("drive_mounted") => "watching",
        Some("webhook") => "listening",
        _ => "ready",
    }
}

pub(crate) fn load_enabled(path: &Path) -> Result<Vec<Workflow>, String> {
    let connection = open(path)?;
    let mut statement = connection
        .prepare("SELECT definition FROM workflows WHERE enabled=1 AND approved_signature=signature")
        .map_err(|error| error.to_string())?;
    let workflows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| error.to_string())?
        .map(|row| {
            let json = row.map_err(|error| error.to_string())?;
            serde_json::from_str(&json).map_err(|error| error.to_string())
        })
        .collect();
    workflows
}

pub(crate) fn statuses(path: &Path) -> Result<Vec<WorkflowRuntimeStatus>, String> {
    let connection = open(path)?;
    let mut statement = connection
        .prepare("SELECT id,enabled,runtime_state,last_error,last_run_at FROM workflows ORDER BY updated_at DESC")
        .map_err(|error| error.to_string())?;
    let statuses = statement
        .query_map([], status_from_row)
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string());
    statuses
}

fn status_for(connection: &Connection, id: &str) -> Result<WorkflowRuntimeStatus, String> {
    connection
        .query_row(
            "SELECT id,enabled,runtime_state,last_error,last_run_at FROM workflows WHERE id=?1",
            [id],
            status_from_row,
        )
        .map_err(|error| error.to_string())
}

fn status_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkflowRuntimeStatus> {
    let state: String = row.get(2)?;
    let error: Option<String> = row.get(3)?;
    Ok(WorkflowRuntimeStatus {
        workflow_id: row.get(0)?,
        enabled: row.get(1)?,
        message: error.clone().unwrap_or_else(|| match state.as_str() {
            "watching" => "Watching for filesystem changes".into(),
            "scheduled" => "Waiting for the next scheduled time".into(),
            "armed" => "Runs once when the background engine starts".into(),
            "listening" => "Listening on a loopback-only webhook".into(),
            "ready" => "Dry run approved; ready to enable".into(),
            "needs_preview" => "A dry run is required".into(),
            _ => "Background execution is paused".into(),
        }),
        state: if error.is_some() { "error".into() } else { state },
        last_run_at: row.get(4)?,
    })
}

pub(crate) fn begin_run(path: &Path, run: &RunRecord, trigger: &str) -> Result<(), String> {
    open(path)?
        .execute(
            "INSERT INTO active_runs VALUES (?1,?2,?3,?4,?5)",
            params![run.id, run.workflow_id, run.workflow_name, run.started_at, trigger],
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

pub(crate) fn finish_run(path: &Path, run: &RunRecord) -> Result<(), String> {
    let mut connection = open(path)?;
    let transaction = connection.transaction().map_err(|error| error.to_string())?;
    transaction
        .execute(
            "INSERT OR REPLACE INTO runs VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![run.id, run.workflow_id, run.workflow_name, run.started_at, run.duration_ms as i64, run.status, run.files as i64, run.reversible, run.message],
        )
        .map_err(|error| error.to_string())?;
    transaction
        .execute("DELETE FROM active_runs WHERE id=?1", [&run.id])
        .map_err(|error| error.to_string())?;
    transaction
        .execute(
            "UPDATE workflows SET last_run_at=?2,runtime_state=CASE WHEN enabled=1 THEN runtime_state ELSE 'ready' END,last_error=NULL WHERE id=?1",
            params![run.workflow_id, run.started_at],
        )
        .map_err(|error| error.to_string())?;
    transaction.commit().map_err(|error| error.to_string())
}

pub(crate) fn record_operation(
    path: &Path,
    run_id: &str,
    sequence: usize,
    action: &str,
    source: Option<&str>,
    destination: Option<&str>,
) -> Result<(), String> {
    open(path)?
        .execute(
            "INSERT OR REPLACE INTO run_operations VALUES (?1,?2,?3,?4,?5,0)",
            params![run_id, sequence as i64, action, source, destination],
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

pub(crate) fn reversible_operations(path: &Path, run_id: &str) -> Result<Vec<ReversibleOperation>, String> {
    let connection = open(path)?;
    let mut statement = connection
        .prepare("SELECT sequence,action,source,destination FROM run_operations WHERE run_id=?1 AND undone=0 ORDER BY sequence DESC")
        .map_err(|error| error.to_string())?;
    let operations = statement
        .query_map([run_id], |row| {
            Ok(ReversibleOperation {
                sequence: row.get(0)?,
                action: row.get(1)?,
                source: row.get(2)?,
                destination: row.get(3)?,
            })
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string());
    operations
}

pub(crate) fn mark_operation_undone(path: &Path, run_id: &str, sequence: i64) -> Result<(), String> {
    open(path)?
        .execute(
            "UPDATE run_operations SET undone=1 WHERE run_id=?1 AND sequence=?2",
            params![run_id, sequence],
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

pub(crate) fn record_failure(path: &Path, workflow: &Workflow, trigger: &str, message: &str) -> Result<(), String> {
    let now = Utc::now().to_rfc3339();
    let connection = open(path)?;
    connection
        .execute(
            "INSERT INTO runs VALUES (?1,?2,?3,?4,0,'failed',0,0,?5)",
            params![uuid::Uuid::new_v4().to_string(), workflow.id, workflow.name, now, format!("{trigger}: {message}")],
        )
        .map_err(|error| error.to_string())?;
    connection
        .execute(
            "UPDATE workflows SET runtime_state='error',last_error=?2,last_run_at=?3 WHERE id=?1",
            params![workflow.id, message, now],
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

pub(crate) fn list_runs(path: &Path) -> Result<Vec<RunRecord>, String> {
    let connection = open(path)?;
    let mut statement = connection
        .prepare("SELECT id,workflow_id,workflow_name,started_at,duration_ms,status,files,reversible,message FROM runs ORDER BY started_at DESC LIMIT 100")
        .map_err(|error| error.to_string())?;
    let runs = statement
        .query_map([], |row| {
            Ok(RunRecord {
                id: row.get(0)?,
                workflow_id: row.get(1)?,
                workflow_name: row.get(2)?,
                started_at: row.get(3)?,
                duration_ms: row.get::<_, i64>(4)? as u128,
                status: row.get(5)?,
                files: row.get::<_, i64>(6)? as usize,
                reversible: row.get(7)?,
                message: row.get(8)?,
            })
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string());
    runs
}

pub(crate) fn was_processed(path: &Path, workflow_id: &str, file: &Path) -> Result<bool, String> {
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos() as i64)
        .unwrap_or_default();
    open(path)?
        .query_row(
            "SELECT 1 FROM processed_files WHERE workflow_id=?1 AND path=?2 AND modified_ns=?3 AND size=?4",
            params![workflow_id, file.display().to_string(), modified, metadata.len() as i64],
            |_| Ok(true),
        )
        .optional()
        .map(|value| value.unwrap_or(false))
        .map_err(|error| error.to_string())
}

pub(crate) fn mark_processed(path: &Path, workflow_id: &str, files: &[std::path::PathBuf]) -> Result<(), String> {
    let mut connection = open(path)?;
    let transaction = connection.transaction().map_err(|error| error.to_string())?;
    for file in files {
        let Ok(metadata) = file.metadata() else { continue };
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos() as i64)
            .unwrap_or_default();
        transaction
            .execute(
                "INSERT OR IGNORE INTO processed_files VALUES (?1,?2,?3,?4,?5)",
                params![workflow_id, file.display().to_string(), modified, metadata.len() as i64, Utc::now().to_rfc3339()],
            )
            .map_err(|error| error.to_string())?;
    }
    transaction.commit().map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{NodeData, WorkflowEdge, WorkflowNode};
    use std::collections::HashMap;

    fn sample_workflow() -> Workflow {
        Workflow {
            id: "test-workflow".into(),
            name: "Test workflow".into(),
            enabled: false,
            permissions: vec![std::env::temp_dir().display().to_string()],
            nodes: vec![
                WorkflowNode {
                    id: "trigger".into(),
                    data: NodeData {
                        label: "Watch temp".into(),
                        kind: "trigger".into(),
                        node_type: "folder_changed".into(),
                        config: HashMap::from([(
                            "path".into(),
                            serde_json::Value::String(std::env::temp_dir().display().to_string()),
                        )]),
                    },
                },
                WorkflowNode {
                    id: "action".into(),
                    data: NodeData {
                        label: "Checksum".into(),
                        kind: "action".into(),
                        node_type: "checksum".into(),
                        config: HashMap::new(),
                    },
                },
            ],
            edges: vec![WorkflowEdge { source: "trigger".into(), target: "action".into() }],
        }
    }

    #[test]
    fn material_edits_revoke_durable_approval() {
        let path = std::env::temp_dir().join(format!("conduit-db-test-{}.sqlite3", uuid::Uuid::new_v4()));
        initialize(&path).unwrap();
        let mut workflow = sample_workflow();
        sync_workflow(&path, &workflow).unwrap();
        assert!(!is_approved(&path, &workflow).unwrap());
        approve_preview(&path, &workflow).unwrap();
        assert!(is_approved(&path, &workflow).unwrap());
        assert!(set_enabled(&path, &workflow, true).unwrap().enabled);

        workflow.nodes[1].data.label = "Changed action".into();
        let status = sync_workflow(&path, &workflow).unwrap();
        assert!(!status.enabled);
        assert_eq!(status.state, "needs_preview");
        assert!(!is_approved(&path, &workflow).unwrap());
        let _ = std::fs::remove_file(path);
    }


    #[test]
    fn schedule_slots_are_claimed_only_once() {
        let directory = std::env::temp_dir().join(format!("conduit-schedule-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("runtime.sqlite3");
        initialize(&path).unwrap();
        assert!(claim_schedule(&path, "workflow", "2026-10-21T18:00").unwrap());
        assert!(!claim_schedule(&path, "workflow", "2026-10-21T18:00").unwrap());
        std::fs::remove_dir_all(directory).unwrap();
    }
}
