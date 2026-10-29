import { invoke } from "@tauri-apps/api/core";
import type { CapabilityStatus, EngineResult, RunRecord, WorkerHealth, Workflow, WorkflowRuntimeStatus } from "../types";

const STORAGE_KEY = "conduit.workflows.v1";
const RUNS_KEY = "conduit.runs.v1";
const BUILTINS_KEY = "conduit.builtins.v7";

export function upgradeBuiltins(workflows: Workflow[], fallback: Workflow[]): Workflow[] {
  const weeklyDefault = fallback.find((workflow) => workflow.id === "weekly-dev-note");
  return workflows.filter((workflow) => workflow.id !== "organise-downloads").map((workflow) => {
    if (
      workflow.id === "weekly-dev-note"
      && !workflow.nodes.some((node) => node.data.nodeType === "git_summary")
      && weeklyDefault
    ) {
      return { ...weeklyDefault, enabled: false, updatedAt: new Date().toISOString() };
    }
    if (
      workflow.id === "weekly-kmail-cleanup-preview"
      && workflow.nodes.some((node) => node.data.nodeType === "kmail_cleanup" && node.data.config.mode === "preview")
    ) {
      const nodes = workflow.nodes.map((node) => {
        if (node.data.nodeType === "kmail_cleanup" && node.data.config.mode === "preview") {
          return {
            ...node,
            data: {
              ...node.data,
              label: "Preview and file KMail",
              description: "Create candidates.csv, then move that exact set to Cleanup Review",
              config: { ...node.data.config, mode: "preview_and_apply" },
            },
          };
        }
        if (node.data.nodeType === "notification") {
          return {
            ...node,
            data: {
              ...node.data,
              label: "Cleanup complete",
              description: "Show completion",
              config: { ...node.data.config, message: "KMail cleanup moved its candidates to Cleanup Review" },
            },
          };
        }
        return node;
      });
      return {
        ...workflow,
        name: "Weekly KMail cleanup",
        description: "Preview conservative candidates every Sunday, then move that exact set to Cleanup Review.",
        enabled: false,
        nodes,
        updatedAt: new Date().toISOString(),
      };
    }
    if (workflow.id === "apply-reviewed-kmail-cleanup") {
      const nodes = workflow.nodes.map((node) => {
        if (node.data.nodeType !== "kmail_cleanup" || node.data.config.mode !== "apply_reviewed") return node;
        const config = { ...node.data.config };
        if (!config.reportPath) delete config.reportPath;
        return {
          ...node,
          data: {
            ...node.data,
            label: "Apply latest preview",
            description: "Use the newest candidates.csv and revalidate messages",
            config,
          },
        };
      });
      return { ...workflow, enabled: false, nodes, updatedAt: new Date().toISOString() };
    }
    if (workflow.id !== "smart-download-organizer-v2") return workflow;

    const permissions = workflow.permissions.includes("~/Programs")
      ? workflow.permissions
      : [...workflow.permissions, "~/Programs"];
    const nodes = workflow.nodes.map((node) => {
      if (node.data.nodeType !== "organize_downloads") return node;
      const config = { ...node.data.config };
      if (!config.softwareRoot || config.softwareRoot === "~/Downloads/Software") {
        config.softwareRoot = "~/Programs";
      }
      if (!config.romsRoot) config.romsRoot = "~/Programs/Roms";
      if (!config.ps4Root) config.ps4Root = "~/Programs/PS4";
      delete config.otherRoot;
      return { ...node, data: { ...node.data, config } };
    });

    return { ...workflow, enabled: false, permissions, nodes, updatedAt: new Date().toISOString() };
  });
}

export const isTauri = () => "__TAURI_INTERNALS__" in window;

export function loadWorkflows(fallback: Workflow[]): Workflow[] {
  try {
    const saved = localStorage.getItem(STORAGE_KEY);
    if (!saved) {
      localStorage.setItem(BUILTINS_KEY, "installed");
      return fallback;
    }
    const parsed = JSON.parse(saved) as Workflow[];
    if (localStorage.getItem(BUILTINS_KEY) !== "installed") {
      const upgraded = upgradeBuiltins(parsed, fallback);
      const ids = new Set(upgraded.map((workflow) => workflow.id));
      const additions = fallback.filter((workflow) => !ids.has(workflow.id));
      localStorage.setItem(BUILTINS_KEY, "installed");
      return [...upgraded, ...additions];
    }
    return parsed;
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
    reversible: !dryRun && operations.every((item) => item.action !== "notification"),
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

export async function getCapabilityStatuses(): Promise<CapabilityStatus[]> {
  if (!isTauri()) return [];
  return invoke<CapabilityStatus[]>("capability_statuses");
}

export async function storeSecret(name: string, secret: string): Promise<void> {
  if (!isTauri()) throw new Error("Secrets are only available in the desktop app");
  await invoke("store_secret", { name, secret });
}

export async function removeSecret(name: string): Promise<void> {
  if (!isTauri()) throw new Error("Secrets are only available in the desktop app");
  await invoke("remove_secret", { name });
}

export async function undoRun(runId: string): Promise<number> {
  if (!isTauri()) return 0;
  return invoke<number>("undo_run", { runId });
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
