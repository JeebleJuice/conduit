import type { Workflow } from "./types";

export const nodeCatalog = [
  { kind: "trigger", nodeType: "manual", label: "Manual run", description: "Start from the toolbar" },
  { kind: "trigger", nodeType: "folder_changed", label: "Folder changed", description: "Watch a folder for new files" },
  { kind: "trigger", nodeType: "schedule", label: "Schedule", description: "Run at a time or interval" },
  { kind: "trigger", nodeType: "startup", label: "App startup", description: "Run when Conduit starts" },
  { kind: "condition", nodeType: "extension", label: "File extension", description: "Continue for matching files" },
  { kind: "condition", nodeType: "file_size", label: "File size", description: "Compare file size to a limit" },
  { kind: "condition", nodeType: "age", label: "File age", description: "Filter by modified time" },
  { kind: "condition", nodeType: "exists", label: "Destination exists", description: "Branch if a path already exists" },
  { kind: "action", nodeType: "create_directory", label: "Create directory", description: "Create a folder when missing" },
  { kind: "action", nodeType: "copy", label: "Copy file", description: "Copy without overwriting" },
  { kind: "action", nodeType: "move", label: "Move file", description: "Move with collision safety" },
  { kind: "action", nodeType: "rename", label: "Rename", description: "Rename from a template" },
  { kind: "action", nodeType: "write_text", label: "Write text", description: "Write or append a template" },
  { kind: "action", nodeType: "checksum", label: "Checksum", description: "Calculate a SHA-256 digest" },
  { kind: "action", nodeType: "notification", label: "Notification", description: "Show a desktop message" },
] as const;

export const initialWorkflows: Workflow[] = [
  {
    id: "organise-downloads",
    name: "Organise downloads",
    description: "Move new PDF downloads into Documents with a safe date prefix.",
    enabled: true,
    updatedAt: new Date().toISOString(),
    permissions: ["~/Downloads", "~/Documents"],
    nodes: [
      { id: "trigger-1", type: "conduit", position: { x: 70, y: 165 }, data: { kind: "trigger", nodeType: "folder_changed", label: "Downloads changed", description: "Watch ~/Downloads", config: { path: "~/Downloads" } } },
      { id: "condition-1", type: "conduit", position: { x: 390, y: 165 }, data: { kind: "condition", nodeType: "extension", label: "File is PDF", description: "Extension equals .pdf", config: { extension: ".pdf" } } },
      { id: "action-1", type: "conduit", position: { x: 710, y: 165 }, data: { kind: "action", nodeType: "move", label: "Move and rename", description: "To ~/Documents/PDFs", config: { destination: "~/Documents/PDFs", template: "{date}-{name}", maxFiles: 50 } } },
      { id: "action-2", type: "conduit", position: { x: 1030, y: 165 }, data: { kind: "action", nodeType: "notification", label: "Show notification", description: "Report moved files", config: { message: "Downloads organised" } } },
    ],
    edges: [
      { id: "e1", source: "trigger-1", target: "condition-1", animated: false },
      { id: "e2", source: "condition-1", target: "action-1", sourceHandle: "true", label: "match", animated: false },
      { id: "e3", source: "action-1", target: "action-2", animated: false },
    ],
  },
  {
    id: "weekly-dev-note",
    name: "Weekly development note",
    description: "Prepare an Obsidian-ready project summary every Friday.",
    enabled: false,
    updatedAt: new Date().toISOString(),
    permissions: ["~/Dev", "~/Documents/Obsidian Notes"],
    nodes: [
      { id: "weekly-trigger", type: "conduit", position: { x: 100, y: 180 }, data: { kind: "trigger", nodeType: "schedule", label: "Friday at 17:00", description: "Weekly schedule", config: { schedule: "0 17 * * 5" } } },
      { id: "weekly-action", type: "conduit", position: { x: 440, y: 180 }, data: { kind: "action", nodeType: "write_text", label: "Write Markdown note", description: "Create the weekly note", config: { destination: "~/Documents/Obsidian Notes/Weekly/{date}.md", append: false } } },
      { id: "weekly-notify", type: "conduit", position: { x: 780, y: 180 }, data: { kind: "action", nodeType: "notification", label: "Note is ready", description: "Show a desktop notification", config: { message: "Weekly note created" } } },
    ],
    edges: [
      { id: "we1", source: "weekly-trigger", target: "weekly-action" },
      { id: "we2", source: "weekly-action", target: "weekly-notify" },
    ],
  },
];
