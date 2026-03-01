import { useState, useEffect } from "react";

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

  useEffect(() => {
    setPrompt(initialPrompt);
    setUseResume(!!resumeSessionId);
  }, [initialPrompt, resumeSessionId]);

  if (!open) return null;

  function handleSubmit() {
    const trimmed = prompt.trim();
    if (!trimmed) return;
    onSubmit(trimmed, useResume && resumeSessionId ? resumeSessionId : undefined);
  }

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/40">
      <div className="bg-white rounded-lg shadow-lg p-6 max-w-lg w-full mx-4">
        <h3 className="text-lg font-semibold text-gray-900">{title}</h3>

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
          rows={12}
          placeholder="Type your prompt here..."
          className="mt-3 w-full rounded-md border border-gray-300 px-3 py-2 text-sm font-mono focus:outline-none focus:ring-2 focus:ring-indigo-500 focus:border-indigo-500 resize-y"
        />

        <div className="mt-4 flex justify-end gap-2">
          <button
            onClick={onCancel}
            className="px-3 py-1.5 text-sm rounded-md border border-gray-300 text-gray-700 hover:bg-gray-50"
          >
            Cancel
          </button>
          <button
            onClick={handleSubmit}
            disabled={!prompt.trim()}
            className="px-3 py-1.5 text-sm rounded-md bg-indigo-600 text-white hover:bg-indigo-700 disabled:opacity-50 disabled:cursor-not-allowed"
          >
            {useResume && resumeSessionId ? "Resume Agent" : "Launch Agent"}
          </button>
        </div>
      </div>
    </div>
  );
}
