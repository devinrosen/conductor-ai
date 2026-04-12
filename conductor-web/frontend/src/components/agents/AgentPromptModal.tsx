import { useState, useEffect, useMemo, useId } from "react";
import { BaseModal } from "../shared/BaseModal";
import type { KnownModel } from "../../api/types";
import { api } from "../../api/client";

/** Client-side keyword heuristics matching conductor-core's suggest_model(). */
const HAIKU_KEYWORDS = [
  "commit", "format", "lint", "rename", "typo", "bump version",
  "changelog", "formatting", "fix typo", "update version",
];
const OPUS_KEYWORDS = [
  "plan", "architect", "design", "refactor", "analyze", "review",
  "implement", "rewrite", "migrate", "complex",
];

/**
 * Fallback used when /config/known-models is unavailable.
 * Mirrors conductor-core/src/models.rs KNOWN_MODELS — update both if models change.
 */
const FALLBACK_MODELS: KnownModel[] = [
  { id: "claude-opus-4-6",           alias: "opus",   tier: 3, tier_label: "Powerful", description: "Planning, architecture, complex analysis" },
  { id: "claude-sonnet-4-6",         alias: "sonnet", tier: 2, tier_label: "Balanced", description: "General implementation (default)" },
  { id: "claude-haiku-4-5-20251001", alias: "haiku",  tier: 1, tier_label: "Fast",     description: "Commit messages, formatting, quick edits" },
];

function suggestModel(prompt: string): string {
  const lower = prompt.toLowerCase();
  for (const kw of HAIKU_KEYWORDS) {
    if (lower.includes(kw)) return "haiku";
  }
  for (const kw of OPUS_KEYWORDS) {
    if (lower.includes(kw)) return "opus";
  }
  return "sonnet";
}

interface AgentPromptModalProps {
  open: boolean;
  title: string;
  initialPrompt: string;
  resumeSessionId: string | null;
  onSubmit: (prompt: string, resumeSessionId?: string) => void;
  onCancel: () => void;
}

export function AgentPromptModal({
  open,
  title,
  initialPrompt,
  resumeSessionId,
  onSubmit,
  onCancel,
}: AgentPromptModalProps) {
  const [prompt, setPrompt] = useState(initialPrompt);
  const [useResume, setUseResume] = useState(!!resumeSessionId);
  const [models, setModels] = useState<KnownModel[]>([]);
  const titleId = useId();

  useEffect(() => {
    setPrompt(initialPrompt);
    setUseResume(!!resumeSessionId);
  }, [initialPrompt, resumeSessionId]);

  useEffect(() => {
    if (open && models.length === 0) {
      api.listKnownModels()
        .then(setModels)
        .catch((err) => {
          console.error("[AgentPromptModal] Failed to load known models, using fallback:", err);
          setModels(FALLBACK_MODELS);
        });
    }
  }, [open, models.length]);

  // Live model suggestion based on prompt text
  const suggested = useMemo(() => suggestModel(prompt), [prompt]);

  function handleSubmit() {
    const trimmed = prompt.trim();
    if (!trimmed) return;
    onSubmit(trimmed, useResume && resumeSessionId ? resumeSessionId : undefined);
  }

  return (
    <BaseModal
      open={open}
      onClose={onCancel}
      titleId={titleId}
      className="bg-white rounded-lg shadow-lg p-6 max-w-lg w-full mx-4 outline-none modal-panel"
    >
      <div>
        <h3 id={titleId} className="text-lg font-semibold text-gray-900">{title}</h3>

        {resumeSessionId && (
          <label className="mt-3 flex items-center gap-2 text-sm text-gray-700">
            <input
              type="checkbox"
              checked={useResume}
              onChange={(e) => setUseResume(e.target.checked)}
              className="rounded border-gray-300"
            />
            Resume previous session
            <span className="text-xs text-gray-400 font-mono truncate max-w-[160px]">
              {resumeSessionId}
            </span>
          </label>
        )}

        <textarea
          value={prompt}
          onChange={(e) => setPrompt(e.target.value)}
          onKeyDown={(e) => {
            if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
              e.preventDefault();
              handleSubmit();
            }
          }}
          rows={10}
          placeholder="Type your prompt here..."
          className="mt-3 w-full rounded-md border border-gray-300 px-3 py-2 text-sm font-mono focus:outline-none focus:ring-2 focus:ring-indigo-500 focus:border-indigo-500 resize-y"
        />

        {/* Model suggestion based on prompt */}
        {models.length > 0 && prompt.trim() && (
          <div className="mt-2 flex items-center gap-2 text-xs text-gray-500">
            <span>Suggested model:</span>
            {models.map((m) => {
              const isSuggested = m.alias === suggested;
              return (
                <span
                  key={m.id}
                  className={`inline-flex items-center px-2 py-0.5 rounded font-medium ${
                    isSuggested
                      ? "bg-green-100 text-green-700 ring-1 ring-green-300"
                      : "bg-gray-100 text-gray-500"
                  }`}
                >
                  {m.alias}
                  {isSuggested && (
                    <span className="ml-1 text-green-600">&larr;</span>
                  )}
                </span>
              );
            })}
            <span className="text-gray-400 italic ml-1">(hint only)</span>
          </div>
        )}

        <div className="mt-4 flex justify-end gap-2">
          <button
            onClick={onCancel}
            className="px-3 py-1.5 text-sm rounded-md border border-gray-300 text-gray-700 hover:bg-gray-50 active:scale-95 transition-transform"
          >
            Cancel
          </button>
          <button
            onClick={handleSubmit}
            disabled={!prompt.trim()}
            className="px-3 py-1.5 text-sm rounded-md bg-indigo-600 text-white hover:bg-indigo-700 hover:brightness-110 active:scale-95 transition-transform disabled:opacity-50 disabled:cursor-not-allowed inline-flex items-center gap-1.5"
          >
            {useResume && resumeSessionId ? "Resume Agent" : "Launch Agent"}
            <kbd className="text-[10px] opacity-70 font-sans">&#8984;&#9166;</kbd>
          </button>
        </div>
      </div>
    </BaseModal>
  );
}
