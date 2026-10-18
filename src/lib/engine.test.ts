import { describe, expect, it } from "vitest";
import { initialWorkflows } from "../data";
import { validateWorkflow } from "./engine";

describe("workflow validation", () => {
  it("accepts the bundled workflow", () => {
    expect(validateWorkflow(initialWorkflows[0])).toEqual([]);
  });

  it("rejects cycles", () => {
    const workflow = structuredClone(initialWorkflows[0]);
    workflow.edges.push({ id: "cycle", source: "action-2", target: "condition-1" });
    expect(validateWorkflow(workflow)).toContain("Cycles are not allowed.");
  });

  it("requires one trigger and one action", () => {
    const workflow = { ...structuredClone(initialWorkflows[0]), nodes: [], edges: [] };
    expect(validateWorkflow(workflow)).toEqual([
      "A workflow needs exactly one trigger.",
      "Add at least one action.",
    ]);
  });
});
