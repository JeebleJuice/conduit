import { ChevronDown, Search } from "lucide-react";
import { useMemo, useState } from "react";
import { nodeCatalog } from "../data";
import type { ConduitNodeData, NodeKind } from "../types";

export function NodeLibrary({ onAdd }: { onAdd: (data: Omit<ConduitNodeData, "config">) => void }) {
  const [query, setQuery] = useState("");
  const results = useMemo(() => nodeCatalog.filter((item) => `${item.label} ${item.description}`.toLowerCase().includes(query.toLowerCase())), [query]);
  return (
    <aside className="node-library">
      <div className="panel-heading"><span>Node library</span><kbd>N</kbd></div>
      <label className="search-box"><Search size={14} /><input value={query} onChange={(e) => setQuery(e.target.value)} placeholder="Search nodes" /></label>
      {(["trigger", "condition", "action"] as NodeKind[]).map((kind) => {
        const items = results.filter((item) => item.kind === kind);
        if (!items.length) return null;
        return <section className="library-section" key={kind}>
          <h3><ChevronDown size={14} />{kind}s <span>{items.length}</span></h3>
          {items.map((item) => <button key={item.nodeType} onClick={() => onAdd(item)} className={`library-item ${kind}`}>
            <span className="library-dot" /><span><strong>{item.label}</strong><small>{item.description}</small></span><b>+</b>
          </button>)}
        </section>;
      })}
    </aside>
  );
}
