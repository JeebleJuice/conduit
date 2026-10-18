import { addEdge, Background, BackgroundVariant, Controls, MiniMap, ReactFlow, ReactFlowProvider, useEdgesState, useNodesState, type Connection, type Edge, type NodeMouseHandler } from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { Activity, ArchiveRestore, Check, ChevronDown, CircleAlert, Copy, Download, Ellipsis, FileClock, FolderLock, History, LayoutDashboard, Pause, Play, Plus, Save, Search, Settings, ShieldCheck, Sparkles, Square, Upload, X } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ConduitNode } from "./components/ConduitNode";
import { Inspector } from "./components/Inspector";
import { NodeLibrary } from "./components/NodeLibrary";
import { initialWorkflows } from "./data";
import { executeWorkflow, getRunHistory, getWorkerHealth, getWorkflowStatuses, loadRuns, loadWorkflows, saveWorkflows, setWorkflowEnabled, stopCurrentRun, syncWorkflowDefinitions, validateWorkflow } from "./lib/engine";
import type { ConduitNodeData, EngineResult, RunRecord, WorkerHealth, Workflow, WorkflowNode, WorkflowRuntimeStatus } from "./types";
import "./styles.css";

const nodeTypes = { conduit: ConduitNode };

function Builder({ workflow, onUpdate, onRun }: { workflow: Workflow; onUpdate: (workflow: Workflow) => void; onRun: (dry: boolean) => void }) {
  const [nodes, setNodes, onNodesChange] = useNodesState<WorkflowNode>(workflow.nodes);
  const [edges, setEdges, onEdgesChange] = useEdgesState(workflow.edges);
  const [selectedId, setSelectedId] = useState<string>();
  const [notice, setNotice] = useState<string>();

  useEffect(() => { setNodes(workflow.nodes); setEdges(workflow.edges); setSelectedId(undefined); }, [workflow.id]);
  useEffect(() => { onUpdate({ ...workflow, nodes, edges, updatedAt: new Date().toISOString() }); }, [nodes, edges]);

  const onConnect = useCallback((connection: Connection) => {
    const target = nodes.find((node) => node.id === connection.target);
    if (target?.data.kind === "trigger") return setNotice("Triggers cannot accept incoming connections.");
    const next = addEdge({ ...connection, animated: false }, edges);
    const issue = validateWorkflow({ ...workflow, nodes, edges: next }).find((item) => item.includes("Cycles"));
    if (issue) return setNotice(issue);
    setEdges(next);
  }, [edges, nodes, workflow]);

  const selected = nodes.find((node) => node.id === selectedId);
  const addNode = (data: Omit<ConduitNodeData, "config">) => {
    if (data.kind === "trigger" && nodes.some((node) => node.data.kind === "trigger")) return setNotice("This workflow already has a trigger.");
    const id = `${data.nodeType}-${crypto.randomUUID().slice(0, 6)}`;
    setNodes((current) => [...current, { id, type: "conduit", position: { x: 300 + Math.random() * 260, y: 100 + Math.random() * 280 }, data: { ...data, config: {} } }]);
    setSelectedId(id);
  };

  const updateNode = (next: WorkflowNode) => setNodes((current) => current.map((node) => node.id === next.id ? next : node));
  const deleteSelected = () => {
    if (!selectedId) return;
    setNodes((current) => current.filter((node) => node.id !== selectedId));
    setEdges((current) => current.filter((edge) => edge.source !== selectedId && edge.target !== selectedId));
    setSelectedId(undefined);
  };
  const onNodeClick: NodeMouseHandler<WorkflowNode> = (_, node) => setSelectedId(node.id);

  return <div className="builder-shell">
    <NodeLibrary onAdd={addNode} />
    <main className="canvas-wrap">
      <div className="canvas-toolbar">
        <div><strong>{workflow.name}</strong><span>{nodes.length} nodes · {edges.length} connections</span></div>
        <div className="run-actions"><button onClick={() => onRun(true)} className="preview-button"><Sparkles size={15} /> Dry run</button><button onClick={() => onRun(false)} className="run-button"><Play size={14} fill="currentColor" /> Run now</button></div>
      </div>
      <ReactFlow nodes={nodes} edges={edges} onNodesChange={onNodesChange} onEdgesChange={onEdgesChange} onConnect={onConnect} onNodeClick={onNodeClick} onPaneClick={() => setSelectedId(undefined)} nodeTypes={nodeTypes} fitView minZoom={0.35} maxZoom={1.6} defaultEdgeOptions={{ style: { strokeWidth: 2, stroke: "#6f756d" } }}>
        <Background variant={BackgroundVariant.Dots} gap={24} size={1.2} color="#3b3e39" />
        <Controls position="bottom-left" showInteractive={false} />
        <MiniMap position="bottom-right" pannable zoomable nodeColor={(node) => node.data?.kind === "trigger" ? "#d5a15d" : node.data?.kind === "condition" ? "#859a79" : "#7f91ad"} maskColor="rgba(15,16,14,.72)" />
      </ReactFlow>
      <div className="canvas-hint"><kbd>⌘</kbd><span>+</span><kbd>K</kbd> command menu</div>
    </main>
    <Inspector node={selected} permissions={workflow.permissions} onChange={updateNode} onDelete={deleteSelected} onClose={() => setSelectedId(undefined)} />
    {notice && <button className="toast" onClick={() => setNotice(undefined)}><CircleAlert size={17} />{notice}<X size={14} /></button>}
  </div>;
}

function RunDialog({ result, dryRun, onClose }: { result?: EngineResult; dryRun: boolean; onClose: () => void }) {
  return <div className="modal-backdrop"><section className="run-dialog">
    <div className="dialog-icon"><ShieldCheck size={27} /></div>
    <button className="dialog-close" onClick={onClose}><X size={18} /></button>
    <span className="eyebrow">{dryRun ? "Dry run complete" : "Run complete"}</span>
    <h2>{dryRun ? "Everything looks safe" : "Workflow finished"}</h2>
    <p>{result?.run.message}</p>
    <div className="operation-list">{result?.operations.map((operation) => <div key={operation.nodeId}><Check size={15} /><span><strong>{operation.nodeLabel}</strong><small>{operation.destination || operation.action}</small></span>{operation.warning && <em>{operation.warning}</em>}</div>)}</div>
    <div className="dialog-summary"><span><b>{result?.operations.length ?? 0}</b> actions</span><span><b>{result?.run.files ?? 0}</b> files</span><span><b>{result?.run.durationMs ?? 0}ms</b> elapsed</span></div>
    <button className="primary-wide" onClick={onClose}>Done</button>
  </section></div>;
}

function HistoryView({ runs }: { runs: RunRecord[] }) {
  const exportLog = () => { const url = URL.createObjectURL(new Blob([JSON.stringify(runs, null, 2)], { type: "application/json" })); const anchor = document.createElement("a"); anchor.href = url; anchor.download = "conduit-run-history.json"; anchor.click(); URL.revokeObjectURL(url); };
  return <div className="history-view"><div className="page-title"><div><span className="eyebrow">Activity</span><h1>Execution history</h1><p>Every run remains inspectable. Conduit never works invisibly.</p></div><button onClick={exportLog}><Download size={15} /> Export log</button></div>
    <div className="history-table"><div className="history-header"><span>Workflow</span><span>Started</span><span>Duration</span><span>Files</span><span>Status</span><span>Undo</span></div>
      {runs.length ? runs.map((run) => <div className="history-row" key={run.id}><span><b>{run.workflowName}</b><small>{run.message}</small></span><span>{new Date(run.startedAt).toLocaleString()}</span><span>{run.durationMs} ms</span><span>{run.files}</span><span><i className={`status-pill ${run.status}`}>{run.status}</i></span><span>{run.reversible ? <button className="undo-button"><ArchiveRestore size={14} /> Review</button> : <em>Not reversible</em>}</span></div>) : <div className="empty-history"><FileClock size={35} /><h3>No runs yet</h3><p>Preview or run a workflow to create its first record.</p></div>}
    </div>
  </div>;
}

function App() {
  const [workflows, setWorkflows] = useState(() => loadWorkflows(initialWorkflows));
  const [activeId, setActiveId] = useState(workflows[0].id);
  const [view, setView] = useState<"builder" | "history">("builder");
  const [runs, setRuns] = useState(loadRuns);
  const [result, setResult] = useState<EngineResult>();
  const [dryRun, setDryRun] = useState(true);
  const [running, setRunning] = useState(false);
  const [runtime, setRuntime] = useState<Record<string, WorkflowRuntimeStatus>>({});
  const [health, setHealth] = useState<WorkerHealth>({ running: false, enabledWorkflows: 0, watchedFolders: 0 });
  const [engineOpen, setEngineOpen] = useState(false);
  const fileInput = useRef<HTMLInputElement>(null);
  const active = workflows.find((workflow) => workflow.id === activeId) ?? workflows[0];

  useEffect(() => {
    saveWorkflows(workflows);
    const timer = window.setTimeout(() => {
      syncWorkflowDefinitions(workflows).then((statuses) => {
        if (!statuses.length) return;
        setRuntime(Object.fromEntries(statuses.map((status) => [status.workflowId, status])));
        setWorkflows((current) => {
          let changed = false;
          const next = current.map((workflow) => {
            const status = statuses.find((item) => item.workflowId === workflow.id);
            if (status && workflow.enabled !== status.enabled) {
              changed = true;
              return { ...workflow, enabled: status.enabled };
            }
            return workflow;
          });
          return changed ? next : current;
        });
      }).catch(() => undefined);
    }, 350);
    return () => window.clearTimeout(timer);
  }, [workflows]);
  useEffect(() => { getRunHistory().then(setRuns); }, [view]);
  const refreshRuntime = useCallback(async () => {
    const [statuses, worker] = await Promise.all([getWorkflowStatuses(), getWorkerHealth()]);
    if (statuses.length) {
      setRuntime(Object.fromEntries(statuses.map((status) => [status.workflowId, status])));
      setWorkflows((current) => {
        let changed = false;
        const next = current.map((workflow) => {
          const status = statuses.find((item) => item.workflowId === workflow.id);
          if (status && workflow.enabled !== status.enabled) {
            changed = true;
            return { ...workflow, enabled: status.enabled };
          }
          return workflow;
        });
        return changed ? next : current;
      });
    }
    setHealth(worker);
  }, []);
  useEffect(() => {
    refreshRuntime().catch(() => undefined);
    const timer = window.setInterval(() => refreshRuntime().catch(() => undefined), 5000);
    return () => window.clearInterval(timer);
  }, [refreshRuntime]);
  const updateWorkflow = useCallback((next: Workflow) => setWorkflows((all) => all.map((item) => item.id === next.id ? next : item)), []);
  const toggle = async (workflow: Workflow) => {
    try {
      const status = await setWorkflowEnabled(workflow, !workflow.enabled);
      setRuntime((current) => ({ ...current, [workflow.id]: status }));
      setWorkflows((all) => all.map((item) => item.id === workflow.id ? { ...item, enabled: status.enabled } : item));
      await refreshRuntime();
    } catch (error) {
      alert(String(error));
    }
  };
  const createWorkflow = () => {
    const workflow: Workflow = { id: crypto.randomUUID(), name: "Untitled workflow", description: "A new local automation", enabled: false, updatedAt: new Date().toISOString(), permissions: [], nodes: [{ id: "manual-trigger", type: "conduit", position: { x: 120, y: 180 }, data: { kind: "trigger", nodeType: "manual", label: "Manual run", description: "Start from the toolbar", config: {} } }], edges: [] };
    setWorkflows((all) => [...all, workflow]); setActiveId(workflow.id); setView("builder");
  };
  const duplicate = () => { const copy = { ...active, id: crypto.randomUUID(), name: `${active.name} copy`, enabled: false, updatedAt: new Date().toISOString() }; setWorkflows((all) => [...all, copy]); setActiveId(copy.id); };
  const exportWorkflow = () => { const url = URL.createObjectURL(new Blob([JSON.stringify(active, null, 2)], { type: "application/json" })); const anchor = document.createElement("a"); anchor.href = url; anchor.download = `${active.name.toLowerCase().replaceAll(/[^a-z0-9]+/g, "-")}.conduit.json`; anchor.click(); URL.revokeObjectURL(url); };
  const importWorkflow = async (file?: File) => { if (!file) return; try { const parsed = JSON.parse(await file.text()) as Workflow; if (!parsed.id || !Array.isArray(parsed.nodes) || !Array.isArray(parsed.edges)) throw new Error(); parsed.id = crypto.randomUUID(); parsed.enabled = false; setWorkflows((all) => [...all, parsed]); setActiveId(parsed.id); setView("builder"); } catch { alert("That file is not a valid Conduit workflow."); } };
  const run = async (preview: boolean) => {
    const issues = validateWorkflow(active); if (issues.length) return alert(issues.join("\n"));
    setRunning(true); setDryRun(preview);
    setWorkflows((all) => all.map((item) => item.id === active.id ? { ...item, nodes: item.nodes.map((node) => ({ ...node, data: { ...node.data, status: "running" } })) } : item));
    try { const runResult = await executeWorkflow(active, preview); setResult(runResult); setRuns(await getRunHistory()); await refreshRuntime(); setWorkflows((all) => all.map((item) => item.id === active.id ? { ...item, nodes: item.nodes.map((node) => ({ ...node, data: { ...node.data, status: runResult.nodeStatuses[node.id] ?? "idle" } })) } : item)); }
    catch (error) { if (!String(error).includes("Run stopped by user")) alert(String(error)); setRuns(await getRunHistory()); }
    finally { setRunning(false); }
  };

  return <div className="app-shell">
    <header className="topbar"><div className="brand"><span className="logo-mark"><i /><i /><i /></span><strong>CONDUIT</strong><small>LOCAL AUTOMATION</small></div><nav><button className={view === "builder" ? "active" : ""} onClick={() => setView("builder")}><LayoutDashboard size={15} /> Builder</button><button className={view === "history" ? "active" : ""} onClick={() => setView("history")}><History size={15} /> History</button></nav><div className="top-actions"><button className={`engine-status ${health.running ? "online" : ""}`} onClick={() => setEngineOpen((open) => !open)}><i /> {health.running ? `${health.enabledWorkflows} active` : "Local only"}</button><button><Settings size={17} /></button>{engineOpen && <div className="engine-popover"><div><Activity size={16} /><span><strong>Background engine</strong><small>{health.running ? "Running in the system tray" : "Available in the desktop app"}</small></span></div><dl><div><dt>Enabled workflows</dt><dd>{health.enabledWorkflows}</dd></div><div><dt>Watched folders</dt><dd>{health.watchedFolders}</dd></div><div><dt>Last reconciliation</dt><dd>{health.lastReconcileAt ? new Date(health.lastReconcileAt).toLocaleTimeString() : "Pending"}</dd></div></dl>{health.lastError && <p>{health.lastError}</p>}</div>}</div></header>
    <div className="workspace-row">
      <aside className="workflow-rail"><div className="rail-heading"><span>Workflows</span><button onClick={createWorkflow}><Plus size={16} /></button></div><label className="rail-search"><Search size={13} /><input placeholder="Find workflow" /></label>
        <div className="workflow-list">{workflows.map((workflow) => { const status = runtime[workflow.id]; return <button key={workflow.id} onClick={() => { setActiveId(workflow.id); setView("builder"); }} className={workflow.id === activeId && view === "builder" ? "active" : ""}><span className="workflow-glyph"><i /><i /><i /></span><span><strong>{workflow.name}</strong><small>{workflow.nodes.length} nodes · {status?.state === "needs_preview" ? "Needs preview" : status?.state ?? (workflow.enabled ? "Active" : "Paused")}</small></span><i className={`enabled-dot ${workflow.enabled ? "on" : ""}`} /></button>; })}</div>
        <div className="rail-footer"><button onClick={() => fileInput.current?.click()}><Upload size={14} /> Import</button><input ref={fileInput} type="file" accept="application/json,.json" hidden onChange={(event) => importWorkflow(event.target.files?.[0])} /><button onClick={exportWorkflow}><Download size={14} /> Export</button></div>
      </aside>
      <section className="content-column">
        {view === "builder" ? <><div className="workflow-header"><div className="workflow-title"><button className={`toggle ${active.enabled ? "on" : ""}`} onClick={() => toggle(active)} aria-label="Toggle workflow"><span /></button><div><div><input value={active.name} onChange={(e) => updateWorkflow({ ...active, name: e.target.value })} /><ChevronDown size={15} /></div><p>{runtime[active.id]?.message ?? (active.enabled ? "Enabled for background execution" : "Paused — manual runs are still available")}</p></div></div><div className="header-actions"><span className="saved"><Save size={13} /> Saved locally</span><button onClick={duplicate}><Copy size={15} /> Duplicate</button><button onClick={() => { const path = window.prompt("Allow this workflow to access a folder", "~/"); if (path?.trim() && !active.permissions.includes(path.trim())) updateWorkflow({ ...active, permissions: [...active.permissions, path.trim()] }); }}><FolderLock size={15} /> Permissions</button><button><Ellipsis size={17} /></button></div></div><Builder key={active.id} workflow={active} onUpdate={updateWorkflow} onRun={run} /></> : <HistoryView runs={runs} />}
      </section>
    </div>
    {running && <div className="running-bar"><span /><Pause size={15} /> Inspecting workflow safely… <button onClick={() => stopCurrentRun()}><Square size={12} fill="currentColor" /> Stop</button></div>}
    {result && <RunDialog result={result} dryRun={dryRun} onClose={() => setResult(undefined)} />}
  </div>;
}

export default function Root() { return <ReactFlowProvider><App /></ReactFlowProvider>; }
