import type { Edge, Node } from "@xyflow/react";

export type NodeKind = "trigger" | "condition" | "action";

export type ConduitNodeData = {
  label: string;
  description: string;
  kind: NodeKind;
  nodeType: string;
  config: Record<string, string | number | boolean>;
  status?: "idle" | "running" | "success" | "skipped" | "failed";
};

export type WorkflowNode = Node<ConduitNodeData, "conduit">;

export type Workflow = {
  id: string;
  name: string;
  description: string;
  enabled: boolean;
  updatedAt: string;
  permissions: string[];
  nodes: WorkflowNode[];
  edges: Edge[];
};

export type PreviewOperation = {
  nodeId: string;
  nodeLabel: string;
  action: string;
  source?: string;
  destination?: string;
  warning?: string;
};

export type RunRecord = {
  id: string;
  workflowId: string;
  workflowName: string;
  startedAt: string;
  durationMs: number;
  status: "success" | "failed" | "stopped" | "preview";
  files: number;
  reversible: boolean;
  message: string;
};

export type EngineResult = {
  run: RunRecord;
  operations: PreviewOperation[];
  nodeStatuses: Record<string, ConduitNodeData["status"]>;
};

export type WorkflowRuntimeStatus = {
  workflowId: string;
  enabled: boolean;
  state: "watching" | "scheduled" | "armed" | "ready" | "paused" | "needs_preview" | "error";
  message: string;
  lastRunAt?: string;
};

export type WorkerHealth = {
  running: boolean;
  enabledWorkflows: number;
  watchedFolders: number;
  lastReconcileAt?: string;
  lastError?: string;
};
