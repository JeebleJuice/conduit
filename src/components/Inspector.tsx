import { FolderLock, ShieldCheck, Trash2, X } from "lucide-react";
import type { WorkflowNode } from "../types";

const fieldMap: Record<string, { key: string; label: string; placeholder: string; type?: string }[]> = {
  folder_changed: [{ key: "path", label: "Folder", placeholder: "~/Downloads" }],
  schedule: [{ key: "schedule", label: "Cron schedule", placeholder: "0 17 * * 5" }],
  extension: [{ key: "extension", label: "Extension", placeholder: ".pdf" }],
  file_size: [{ key: "maxMb", label: "Maximum MB", placeholder: "25", type: "number" }],
  age: [{ key: "hours", label: "Older than (hours)", placeholder: "24", type: "number" }],
  exists: [{ key: "path", label: "Path", placeholder: "~/Documents/file.pdf" }],
  create_directory: [{ key: "destination", label: "Directory", placeholder: "~/Documents/Archive" }],
  copy: [{ key: "source", label: "Source", placeholder: "~/Downloads/report.pdf" }, { key: "destination", label: "Destination folder", placeholder: "~/Documents" }],
  move: [{ key: "source", label: "Source (optional)", placeholder: "From trigger" }, { key: "destination", label: "Destination folder", placeholder: "~/Documents" }, { key: "maxFiles", label: "Maximum files", placeholder: "50", type: "number" }],
  rename: [{ key: "source", label: "Source", placeholder: "From previous node" }, { key: "template", label: "Name template", placeholder: "{date}-{name}" }],
  write_text: [{ key: "destination", label: "File", placeholder: "~/Notes/{date}.md" }, { key: "content", label: "Template", placeholder: "# Weekly note" }],
  checksum: [{ key: "source", label: "File", placeholder: "From previous node" }],
  notification: [{ key: "message", label: "Message", placeholder: "Workflow complete" }],
};

export function Inspector({ node, permissions, onChange, onDelete, onClose }: { node?: WorkflowNode; permissions: string[]; onChange: (node: WorkflowNode) => void; onDelete: () => void; onClose: () => void }) {
  if (!node) return <aside className="inspector empty-inspector"><ShieldCheck size={30} /><h2>Safe by design</h2><p>Select a node to configure it. Conduit only resolves paths inside this workflow’s approved folders.</p><div className="permission-list"><div><FolderLock size={15} /> Folder access</div>{permissions.map((path) => <code key={path}>{path}</code>)}</div></aside>;
  const fields = fieldMap[node.data.nodeType] ?? [];
  return <aside className="inspector">
    <div className="inspector-head"><div><span>{node.data.kind}</span><h2>{node.data.label}</h2></div><button aria-label="Close inspector" onClick={onClose}><X size={17} /></button></div>
    <label>Display name<input value={node.data.label} onChange={(e) => onChange({ ...node, data: { ...node.data, label: e.target.value } })} /></label>
    {fields.map((field) => <label key={field.key}>{field.label}<input type={field.type ?? "text"} value={String(node.data.config[field.key] ?? "")} placeholder={field.placeholder} onChange={(e) => onChange({ ...node, data: { ...node.data, config: { ...node.data.config, [field.key]: field.type === "number" ? Number(e.target.value) : e.target.value } } })} /></label>)}
    <div className="safety-note"><ShieldCheck size={16} /><div><strong>Guardrails active</strong><p>Overwrite is off. Paths are checked again before every run.</p></div></div>
    <button className="delete-node" onClick={onDelete}><Trash2 size={15} /> Delete node</button>
  </aside>;
}
