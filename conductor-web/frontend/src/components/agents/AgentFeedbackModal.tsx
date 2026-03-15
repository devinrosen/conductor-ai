import { useState, useEffect } from "react";
import { api } from "../../api/client";
import type { FeedbackRequest } from "../../api/types";

interface AgentFeedbackModalProps {
  worktreeId: string;
  open: boolean;
  onClose: () => void;
  onSubmitted: () => void;
}

export function AgentFeedbackModal({
  worktreeId,
  open,
  onClose,
  onSubmitted,
}: AgentFeedbackModalProps) {
  const [feedback, setFeedback] = useState<FeedbackRequest | null>(null);
  const [response, setResponse] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!open) {
      setFeedback(null);
      setResponse("");
      setError(null);
      return;
    }
    api.getPendingFeedback(worktreeId).then(setFeedback).catch(() => {
      setError("Failed to load feedback request");
    });
  }, [open, worktreeId]);

  async function handleSubmit() {
    if (!feedback || !response.trim()) return;
    setSubmitting(true);
    setError(null);
    try {
      await api.submitFeedback(worktreeId, feedback.id, response.trim());
      onSubmitted();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to submit feedback");
    } finally {
      setSubmitting(false);
    }
  }

  async function handleDismiss() {
    if (!feedback) {
      onClose();
      return;
    }
    try {
      await api.dismissFeedback(worktreeId, feedback.id);
    } catch {
      // ignore dismiss errors
    }
    onClose();
  }

  if (!open) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-4">
      <div className="bg-white rounded-lg shadow-lg max-w-lg w-full mx-4">
        <div className="px-6 py-4 border-b border-gray-200">
          <h3 className="text-lg font-semibold text-gray-900">
            Agent Awaiting Feedback
          </h3>
          <p className="text-sm text-gray-500 mt-1">
            The agent has paused and is asking for your input.
          </p>
        </div>

        <div className="px-6 py-4 space-y-4">
          {error && (
            <div className="px-3 py-2 text-sm text-red-700 bg-red-50 rounded-md border border-red-200">
              {error}
            </div>
          )}

          {feedback ? (
            <>
              <div>
                <label className="block text-sm font-medium text-gray-700 mb-1">
                  Agent's question
                </label>
                <div className="w-full px-3 py-2 text-sm bg-gray-50 border border-gray-200 rounded-md text-gray-800 whitespace-pre-wrap">
                  {feedback.prompt}
                </div>
              </div>

              <div>
                <label className="block text-sm font-medium text-gray-700 mb-1">
                  Your response
                </label>
                <textarea
                  value={response}
                  onChange={(e) => setResponse(e.target.value)}
                  placeholder="Type your response..."
                  rows={4}
                  className="w-full px-3 py-2 text-sm border border-gray-300 rounded-md focus:outline-none focus:ring-2 focus:ring-indigo-500 focus:border-indigo-500 resize-none"
                  autoFocus
                />
              </div>
            </>
          ) : !error ? (
            <p className="text-sm text-gray-500">Loading feedback request...</p>
          ) : null}
        </div>

        <div className="px-6 py-4 border-t border-gray-200 flex justify-end gap-2">
          <button
            onClick={handleDismiss}
            disabled={submitting}
            className="px-4 py-2 text-sm rounded-md border border-gray-300 text-gray-700 hover:bg-gray-50 disabled:opacity-50"
          >
            Dismiss
          </button>
          <button
            onClick={handleSubmit}
            disabled={submitting || !feedback || !response.trim()}
            className="px-4 py-2 text-sm rounded-md bg-indigo-600 text-white hover:bg-indigo-700 disabled:opacity-50"
          >
            {submitting ? "Submitting..." : "Submit"}
          </button>
        </div>
      </div>
    </div>
  );
}
