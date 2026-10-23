import { describe, expect, it } from "vitest";
import { initialWorkflows } from "../data";
import { upgradeBuiltins, validateWorkflow } from "./engine";

describe("workflow validation", () => {
  it("accepts the bundled workflow", () => {
    expect(validateWorkflow(initialWorkflows[0])).toEqual([]);
  });

  it("rejects cycles", () => {
    const workflow = structuredClone(initialWorkflows[0]);
    const trigger = workflow.nodes.find((node) => node.data.kind === "trigger")!;
    const finalNode = workflow.nodes.at(-1)!;
    workflow.edges.push({ id: "cycle", source: finalNode.id, target: trigger.id });
    expect(validateWorkflow(workflow)).toContain("Cycles are not allowed.");
  });

  it("requires one trigger and one action", () => {
    const workflow = { ...structuredClone(initialWorkflows[0]), nodes: [], edges: [] };
    expect(validateWorkflow(workflow)).toEqual([
      "A workflow needs exactly one trigger.",
      "Add at least one action.",
    ]);
  });

  it("retires the legacy download workflow and upgrades the placeholder weekly note", () => {
    const placeholder = structuredClone(initialWorkflows.find((workflow) => workflow.id === "weekly-dev-note")!);
    placeholder.nodes = placeholder.nodes.filter((node) => node.data.nodeType !== "git_summary");
    const legacyDownload = { ...structuredClone(initialWorkflows[0]), id: "organise-downloads" };
    const upgraded = upgradeBuiltins([legacyDownload, placeholder], initialWorkflows);
    expect(upgraded.some((workflow) => workflow.id === "organise-downloads")).toBe(false);
    expect(upgraded.find((workflow) => workflow.id === "weekly-dev-note")?.enabled).toBe(false);
    expect(upgraded.find((workflow) => workflow.id === "weekly-dev-note")?.nodes.some(
      (node) => node.data.nodeType === "git_summary",
    )).toBe(true);
  });

  it("upgrades the scheduled KMail preview to preview and apply", () => {
    const current = structuredClone(initialWorkflows.find((workflow) => workflow.id === "weekly-kmail-cleanup-preview")!);
    const cleanup = current.nodes.find((node) => node.data.nodeType === "kmail_cleanup")!;
    cleanup.data.config.mode = "preview";
    current.enabled = true;
    const [upgraded] = upgradeBuiltins([current], initialWorkflows);
    expect(upgraded.enabled).toBe(false);
    expect(upgraded.name).toBe("Weekly KMail cleanup");
    expect(upgraded.nodes.find((node) => node.data.nodeType === "kmail_cleanup")?.data.config.mode)
      .toBe("preview_and_apply");
  });
});
