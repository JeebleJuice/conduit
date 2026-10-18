import { Handle, Position, type NodeProps } from "@xyflow/react";
import { Bell, CalendarClock, CheckCircle2, Copy, FileCheck2, FileInput, FileText, Filter, FolderInput, FolderPlus, Gauge, HardDrive, MousePointer2, Pencil, Play, RotateCw } from "lucide-react";
import type { WorkflowNode } from "../types";

const icons: Record<string, typeof Play> = {
  manual: MousePointer2, folder_changed: FolderInput, schedule: CalendarClock, startup: Play,
  extension: FileCheck2, file_size: Gauge, age: RotateCw, exists: HardDrive,
  create_directory: FolderPlus, copy: Copy, move: FileInput, rename: Pencil,
  write_text: FileText, checksum: CheckCircle2, notification: Bell,
};

export function ConduitNode({ data, selected }: NodeProps<WorkflowNode>) {
  const Icon = icons[data.nodeType] ?? Filter;
  return (
    <div className={`node-card node-${data.kind} status-${data.status ?? "idle"} ${selected ? "selected" : ""}`}>
      {data.kind !== "trigger" && <Handle type="target" position={Position.Left} />}
      <div className="node-kind">{data.kind}</div>
      <div className="node-main">
        <span className="node-icon"><Icon size={17} strokeWidth={1.8} /></span>
        <div><strong>{data.label}</strong><small>{data.description}</small></div>
      </div>
      {data.kind === "condition" ? (
        <>
          <Handle id="true" type="source" position={Position.Right} style={{ top: "42%" }} />
          <Handle id="false" type="source" position={Position.Right} style={{ top: "72%" }} />
          <span className="branch-label true">yes</span><span className="branch-label false">no</span>
        </>
      ) : data.kind !== "action" || data.nodeType !== "notification" ? <Handle type="source" position={Position.Right} /> : null}
      {data.status === "running" && <span className="node-pulse" />}
    </div>
  );
}
