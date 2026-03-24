import type { WorkflowDef, WorkflowNode, AgentRef, Condition } from "../../api/types";

function agentLabel(agent: AgentRef): string {
  return agent.value;
}

function conditionLabel(c: Condition): string {
  if (c.kind === "step_marker") return `${c.step}.${c.marker}`;
  if (c.kind === "bool_input") return `input:${c.input}`;
  return "?";
}

const NODE_ICONS: Record<string, { icon: string; color: string }> = {
  call: { icon: "\u2192", color: "text-green-600" },
  call_workflow: { icon: "\u21B3", color: "text-blue-600" },
  gate: { icon: "\u2295", color: "text-yellow-600" },
  if: { icon: "if", color: "text-purple-600" },
  unless: { icon: "unless", color: "text-purple-600" },
  while: { icon: "while", color: "text-purple-600" },
  do_while: { icon: "do-while", color: "text-purple-600" },
  do: { icon: "do", color: "text-gray-600" },
  parallel: { icon: "\u2551", color: "text-cyan-600" },
  always: { icon: "always", color: "text-orange-600" },
  script: { icon: "$", color: "text-gray-500" },
};

function NodeLine({ node, depth }: { node: WorkflowNode; depth: number }) {
  const style = NODE_ICONS[node.type] ?? { icon: "?", color: "text-gray-400" };
  const indent = depth * 20;

  const renderChildren = (children: WorkflowNode[]) =>
    children.map((child, i) => <NodeLine key={i} node={child} depth={depth + 1} />);

  let label: string;
  let annotation: string | null = null;
  let children: WorkflowNode[] = [];

  switch (node.type) {
    case "call":
      label = agentLabel(node.agent);
      if (node.retries > 0) annotation = `retries: ${node.retries}`;
      break;
    case "call_workflow":
      label = node.workflow;
      if (node.retries > 0) annotation = `retries: ${node.retries}`;
      break;
    case "gate":
      label = `${node.name} (${node.gate_type})`;
      if (node.prompt) annotation = `"${node.prompt.slice(0, 60)}${node.prompt.length > 60 ? "..." : ""}"`;
      break;
    case "if":
      label = conditionLabel(node.condition);
      children = node.body;
      break;
    case "unless":
      label = conditionLabel(node.condition);
      children = node.body;
      break;
    case "while":
      label = `${node.step}.${node.marker} (max ${node.max_iterations})`;
      children = node.body;
      break;
    case "do_while":
      label = `${node.step}.${node.marker} (max ${node.max_iterations})`;
      children = node.body;
      break;
    case "do":
      label = "";
      children = node.body;
      break;
    case "parallel":
      label = node.calls.map(agentLabel).join(", ");
      if (node.min_success != null) annotation = `min_success: ${node.min_success}`;
      break;
    case "always":
      label = "";
      children = node.body;
      break;
    case "script":
      label = `${node.name}: ${node.run}`;
      if (node.retries > 0) annotation = `retries: ${node.retries}`;
      break;
    default:
      label = "unknown";
  }

  return (
    <>
      <div className="flex items-start gap-1.5 py-0.5" style={{ paddingLeft: indent }}>
        <span className={`font-mono text-xs font-bold shrink-0 ${style.color}`}>
          {style.icon}
        </span>
        <span className="text-sm text-gray-800">
          {label}
        </span>
        {annotation && (
          <span className="text-xs text-gray-400 ml-1">{annotation}</span>
        )}
      </div>
      {children.length > 0 && renderChildren(children)}
    </>
  );
}

interface WorkflowDefViewerProps {
  def: WorkflowDef;
}

export function WorkflowDefViewer({ def }: WorkflowDefViewerProps) {
  return (
    <div className="flex flex-col lg:flex-row gap-6">
      {/* Left: Metadata */}
      <div className="lg:w-1/3 space-y-4">
        <div>
          <h3 className="text-lg font-bold text-gray-900">{def.name}</h3>
          {def.description && (
            <p className="text-sm text-gray-500 mt-1">{def.description}</p>
          )}
        </div>

        <div className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-sm">
          <span className="text-gray-400">Trigger</span>
          <span className="text-gray-700">{def.trigger}</span>
          <span className="text-gray-400">Source</span>
          <span className="text-gray-600 font-mono text-xs truncate" title={def.source_path}>
            {def.source_path}
          </span>
        </div>

        {def.targets.length > 0 && (
          <div>
            <h4 className="text-xs font-medium text-gray-500 uppercase tracking-wider mb-1">Targets</h4>
            <div className="flex flex-wrap gap-1">
              {def.targets.map((t) => (
                <span key={t} className="text-xs px-2 py-0.5 bg-gray-100 text-gray-600 rounded">{t}</span>
              ))}
            </div>
          </div>
        )}

        {def.inputs.length > 0 && (
          <div>
            <h4 className="text-xs font-medium text-gray-500 uppercase tracking-wider mb-1">Inputs</h4>
            <div className="space-y-2">
              {def.inputs.map((inp) => (
                <div key={inp.name} className="text-sm">
                  <div className="flex items-center gap-1">
                    <span className="font-medium text-gray-800">{inp.name}</span>
                    <span className="text-[10px] px-1 py-0.5 rounded bg-gray-100 text-gray-500">{inp.input_type}</span>
                    {inp.required && <span className="text-red-400 text-xs">*</span>}
                  </div>
                  {inp.description && (
                    <p className="text-xs text-gray-400 mt-0.5">{inp.description}</p>
                  )}
                  {inp.default != null && (
                    <p className="text-xs text-gray-400 italic">default: {inp.default}</p>
                  )}
                </div>
              ))}
            </div>
          </div>
        )}
      </div>

      {/* Right: AST Tree */}
      <div className="lg:w-2/3">
        <h4 className="text-xs font-medium text-gray-500 uppercase tracking-wider mb-2">Steps</h4>
        <div className="rounded-lg border border-gray-200 bg-white p-3 overflow-x-auto">
          {def.body.map((node, i) => (
            <NodeLine key={`body-${i}`} node={node} depth={0} />
          ))}
          {def.always.length > 0 && (
            <>
              <div className="border-t border-gray-200 my-2" />
              <div className="text-xs font-medium text-orange-600 mb-1">always</div>
              {def.always.map((node, i) => (
                <NodeLine key={`always-${i}`} node={node} depth={0} />
              ))}
            </>
          )}
        </div>
      </div>
    </div>
  );
}
