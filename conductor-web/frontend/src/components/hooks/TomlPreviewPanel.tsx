import { useState } from "react";
import type { HookSummary } from "../../api/types";

interface TomlPreviewPanelProps {
  hooks: HookSummary[];
  /** Pending on-pattern overrides keyed by hook index */
  draft: Map<number, string>;
}

function tomlString(hooks: HookSummary[], draft: Map<number, string>): string {
  if (hooks.length === 0) return "# No hooks configured";

  return hooks
    .map((hook) => {
      const on = draft.get(hook.index) ?? hook.on;
      const lines = [`[[notify.hooks]]`, `on = "${on}"`];
      if (hook.kind === "shell" && hook.command) {
        lines.push(`run = "${hook.command.replace(/\\/g, "\\\\").replace(/"/g, '\\"')}"`);
      } else if (hook.kind === "http" && hook.command) {
        lines.push(`url = "${hook.command.replace(/\\/g, "\\\\").replace(/"/g, '\\"')}"`);
      }
      return lines.join("\n");
    })
    .join("\n\n");
}

export function TomlPreviewPanel({ hooks, draft }: TomlPreviewPanelProps) {
  const [open, setOpen] = useState(false);

  return (
    <details
      open={open}
      onToggle={(e) => setOpen((e.currentTarget as HTMLDetailsElement).open)}
      className="border border-gray-200 rounded-md overflow-hidden"
    >
      <summary className="px-3 py-2 text-xs font-medium text-gray-500 bg-gray-50 cursor-pointer hover:bg-gray-100 select-none">
        TOML preview
      </summary>
      {open && (
        <pre className="px-3 py-2 text-xs font-mono text-gray-700 bg-white overflow-x-auto whitespace-pre-wrap">
          <code>{tomlString(hooks, draft)}</code>
        </pre>
      )}
    </details>
  );
}
