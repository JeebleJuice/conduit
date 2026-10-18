use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Workflow {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub nodes: Vec<WorkflowNode>,
    #[serde(default)]
    pub edges: Vec<WorkflowEdge>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct WorkflowNode {
    pub id: String,
    pub data: NodeData,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NodeData {
    pub label: String,
    pub kind: String,
    pub node_type: String,
    #[serde(default)]
    pub config: HashMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkflowEdge {
    pub source: String,
    pub target: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PreviewOperation {
    pub node_id: String,
    pub node_label: String,
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RunRecord {
    pub id: String,
    pub workflow_id: String,
    pub workflow_name: String,
    pub started_at: String,
    pub duration_ms: u128,
    pub status: String,
    pub files: usize,
    pub reversible: bool,
    pub message: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EngineResult {
    pub run: RunRecord,
    pub operations: Vec<PreviewOperation>,
    pub node_statuses: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkflowRuntimeStatus {
    pub workflow_id: String,
    pub enabled: bool,
    pub state: String,
    pub message: String,
    pub last_run_at: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkerHealth {
    pub running: bool,
    pub enabled_workflows: usize,
    pub watched_folders: usize,
    pub last_reconcile_at: Option<String>,
    pub last_error: Option<String>,
}
