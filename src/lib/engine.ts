import { invoke } from "@tauri-apps/api/core";
import type { EngineResult, RunRecord, WorkerHealth, Workflow, WorkflowRuntimeStatus } from "../types";

const STORAGE_KEY = "conduit.workflows.v1";
const RUNS_KEY = "conduit.runs.v1";

export const isTauri = () => "__TAURI_INTERNALS__" in window;

export function loadWorkflows(fallback: Workflow[]): Workflow[] {
  try {
    const saved = localStorage.getItem(STORAGE_KEY);
    return saved ? JSON.parse(saved) : fallback;
  } catch {
    return fallback;
  }
}

export function saveWorkflows(workflows: Workflow[]) {
  localStorage.setItem(STORAGE_KEY, JSON.stringify(workflows));
}

export function loadRuns(): RunRecord[] {
  try {
    return JSON.parse(localStorage.getItem(RUNS_KEY) ?? "[]");
  } catch {
    return [];
  }
}

export async function getRunHistory(): Promise<RunRecord[]> {
  if (!isTauri()) return loadRuns();
  try {
    return await invoke<RunRecord[]>("list_run_history");
  } catch {
    return loadRuns();
  }
}

function saveRun(run: RunRecord) {
  const runs = [run, ...loadRuns()].slice(0, 100);
  localStorage.setItem(RUNS_KEY, JSON.stringify(runs));
}

function simulate(workflow: Workflow, dryRun: boolean): EngineResult {
  const started = new Date();
  const operations = workflow.nodes
    .filter((node) => node.data.kind === "action")
    .map((node) => ({
      nodeId: node.id,
      nodeLabel: node.data.label,
      action: node.data.nodeType.replaceAll("_", " "),
      destination: String(node.data.config.destination ?? ""),
      warning: node.data.nodeType === "move" ? "Original files would be moved" : undefined,
    }));
  const run: RunRecord = {
    id: crypto.randomUUID(),
    workflowId: workflow.id,
    workflowName: workflow.name,
    startedAt: started.toISOString(),
    durationMs: 420 + operations.length * 80,
    status: dryRun ? "preview" : "success",
    files: dryRun ? Math.max(operations.length, 1) : 0,
    reversible: operations.every((item) => item.action !== "notification"),
    message: dryRun ? `${operations.length} action${operations.length === 1 ? "" : "s"} checked safely` : "Simulation complete — use the desktop build for filesystem execution",
  };
  return { run, operations, nodeStatuses: Object.fromEntries(workflow.nodes.map((node) => [node.id, "success"])) };
}

export async function executeWorkflow(workflow: Workflow, dryRun: boolean): Promise<EngineResult> {
  const result = isTauri()
    ? await invoke<EngineResult>("execute_workflow", { workflow, dryRun })
    : simulate(workflow, dryRun);
  saveRun(result.run);
  return result;
}

export async function syncWorkflowDefinitions(workflows: Workflow[]): Promise<WorkflowRuntimeStatus[]> {
  if (!isTauri()) {
    return workflows.map((workflow) => ({
      workflowId: workflow.id,
      enabled: workflow.enabled,
      state: workflow.enabled ? "watching" : "paused",
      message: workflow.enabled ? "Browser simulation is active" : "Background execution is paused",
    }));
  }
  return invoke<WorkflowRuntimeStatus[]>("sync_workflows", { workflows });
}

export async function setWorkflowEnabled(workflow: Workflow, enabled: boolean): Promise<WorkflowRuntimeStatus> {
  if (!isTauri()) {
    return {
      workflowId: workflow.id,
      enabled,
      state: enabled ? "watching" : "paused",
      message: enabled ? "Browser simulation is active" : "Background execution is paused",
    };
  }
  return invoke<WorkflowRuntimeStatus>("set_workflow_enabled", { workflow, enabled });
}

export async function getWorkflowStatuses(): Promise<WorkflowRuntimeStatus[]> {
  if (!isTauri()) return [];
  return invoke<WorkflowRuntimeStatus[]>("workflow_statuses");
}

export async function getWorkerHealth(): Promise<WorkerHealth> {
  if (!isTauri()) return { running: false, enabledWorkflows: 0, watchedFolders: 0 };
  return invoke<WorkerHealth>("worker_health");
}

export async function stopCurrentRun(): Promise<void> {
  if (isTauri()) await invoke("stop_current_run");
}

export function validateWorkflow(workflow: Workflow): string[] {
  const issues: string[] = [];
  const triggers = workflow.nodes.filter((node) => node.data.kind === "trigger");
  if (triggers.length !== 1) issues.push("A workflow needs exactly one trigger.");
  if (!workflow.nodes.some((node) => node.data.kind === "action")) issues.push("Add at least one action.");
  const visited = new Set<string>();
  const active = new Set<string>();
  const adjacency = new Map<string, string[]>();
  workflow.edges.forEach((edge) => adjacency.set(edge.source, [...(adjacency.get(edge.source) ?? []), edge.target]));
  const hasCycle = (id: string): boolean => {
    if (active.has(id)) return true;
    if (visited.has(id)) return false;
    visited.add(id); active.add(id);
    const cycle = (adjacency.get(id) ?? []).some(hasCycle);
    active.delete(id);
    return cycle;
  };
  if (workflow.nodes.some((node) => hasCycle(node.id))) issues.push("Cycles are not allowed.");
  const ids = new Set(workflow.nodes.map((node) => node.id));
  if (workflow.edges.some((edge) => !ids.has(edge.source) || !ids.has(edge.target))) issues.push("A connection points to a missing node.");
  return issues;
}
