import { useState, useEffect, useRef, useCallback } from "react";
import { BaseModal, useModalTitleId } from "../shared/BaseModal";
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
  const [selectedValues, setSelectedValues] = useState<Set<string>>(new Set());
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [remainingSecs, setRemainingSecs] = useState<number | null>(null);
  const timerRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const titleId = useModalTitleId();

  const handleDismiss = useCallback(async () => {
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
  }, [feedback, worktreeId, onClose]);

  useEffect(() => {
    if (!open) {
      setFeedback(null);
      setResponse("");
      setSelectedValues(new Set());
      setError(null);
      setRemainingSecs(null);
      if (timerRef.current) clearInterval(timerRef.current);
      return;
    }
    api.getPendingFeedback(worktreeId).then((fb) => {
      setFeedback(fb);
      if (fb?.timeout_secs) {
        const createdAt = new Date(fb.created_at).getTime();
        const expiresAt = createdAt + fb.timeout_secs * 1000;
        const update = () => {
          const left = Math.max(0, Math.ceil((expiresAt - Date.now()) / 1000));
          setRemainingSecs(left);
          if (left <= 0 && timerRef.current) {
            clearInterval(timerRef.current);
          }
        };
        update();
        timerRef.current = setInterval(update, 1000);
      }
    }).catch(() => {
      setError("Failed to load feedback request");
    });
    return () => {
      if (timerRef.current) clearInterval(timerRef.current);
    };
  }, [open, worktreeId]);

  function buildResponseValue(): string {
    const ft = feedback?.feedback_type ?? "text";
    if (ft === "confirm") return response;
    if (ft === "single_select") return response;
    if (ft === "multi_select") return JSON.stringify([...selectedValues]);
    return response.trim();
  }

  function canSubmit(): boolean {
    if (!feedback || submitting) return false;
    const ft = feedback.feedback_type ?? "text";
    if (ft === "text") return !!response.trim();
    if (ft === "confirm") return response === "yes" || response === "no";
    if (ft === "single_select") return !!response;
    if (ft === "multi_select") return selectedValues.size > 0;
    return !!response.trim();
  }

  async function handleSubmit() {
    if (!feedback || !canSubmit()) return;
    setSubmitting(true);
    setError(null);
    try {
      await api.submitFeedback(worktreeId, feedback.id, buildResponseValue());
      onSubmitted();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to submit feedback");
    } finally {
      setSubmitting(false);
    }
  }

  function toggleMultiSelect(value: string) {
    setSelectedValues((prev) => {
      const next = new Set(prev);
      if (next.has(value)) next.delete(value);
      else next.add(value);
      return next;
    });
  }

  const ft = feedback?.feedback_type ?? "text";

  return (
    <BaseModal
      open={open}
      onClose={handleDismiss}
      className="bg-white rounded-lg shadow-lg max-w-lg w-full mx-4 outline-none modal-panel"
    >
      <div>
        <div className="px-6 py-4 border-b border-gray-200">
          <h3 id={titleId} className="text-lg font-semibold text-gray-900">
            Agent Awaiting Feedback
          </h3>
          <p className="text-sm text-gray-500 mt-1">
            The agent has paused and is asking for your input.
          </p>
          {remainingSecs !== null && remainingSecs > 0 && (
            <p className="text-xs text-amber-600 mt-1">
              Auto-dismiss in {remainingSecs}s
            </p>
          )}
          {remainingSecs !== null && remainingSecs <= 0 && (
            <p className="text-xs text-red-600 mt-1">
              Timeout expired — this request may have been dismissed.
            </p>
          )}
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

              {/* Text input */}
              {ft === "text" && (
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
              )}

              {/* Confirm (Yes/No) */}
              {ft === "confirm" && (
                <div className="flex gap-3">
                  <button
                    onClick={() => setResponse("yes")}
                    className={`flex-1 px-4 py-2 text-sm rounded-md border ${response === "yes" ? "bg-green-100 border-green-500 text-green-800" : "border-gray-300 text-gray-700 hover:bg-gray-50"}`}
                  >
                    Yes
                  </button>
                  <button
                    onClick={() => setResponse("no")}
                    className={`flex-1 px-4 py-2 text-sm rounded-md border ${response === "no" ? "bg-red-100 border-red-500 text-red-800" : "border-gray-300 text-gray-700 hover:bg-gray-50"}`}
                  >
                    No
                  </button>
                </div>
              )}

              {/* Single select (radio buttons) */}
              {ft === "single_select" && feedback.options && (
                <div className="space-y-2">
                  <label className="block text-sm font-medium text-gray-700">
                    Select one
                  </label>
                  {feedback.options.map((opt) => (
                    <label
                      key={opt.value}
                      className={`flex items-center gap-2 px-3 py-2 rounded-md border cursor-pointer ${response === opt.value ? "bg-indigo-50 border-indigo-500" : "border-gray-200 hover:bg-gray-50"}`}
                    >
                      <input
                        type="radio"
                        name="feedback-select"
                        value={opt.value}
                        checked={response === opt.value}
                        onChange={() => setResponse(opt.value)}
                        className="text-indigo-600"
                      />
                      <span className="text-sm text-gray-800">{opt.label}</span>
                    </label>
                  ))}
                </div>
              )}

              {/* Multi select (checkboxes) */}
              {ft === "multi_select" && feedback.options && (
                <div className="space-y-2">
                  <label className="block text-sm font-medium text-gray-700">
                    Select one or more
                  </label>
                  {feedback.options.map((opt) => (
                    <label
                      key={opt.value}
                      className={`flex items-center gap-2 px-3 py-2 rounded-md border cursor-pointer ${selectedValues.has(opt.value) ? "bg-indigo-50 border-indigo-500" : "border-gray-200 hover:bg-gray-50"}`}
                    >
                      <input
                        type="checkbox"
                        value={opt.value}
                        checked={selectedValues.has(opt.value)}
                        onChange={() => toggleMultiSelect(opt.value)}
                        className="text-indigo-600"
                      />
                      <span className="text-sm text-gray-800">{opt.label}</span>
                    </label>
                  ))}
                </div>
              )}
            </>
          ) : !error ? (
            <p className="text-sm text-gray-500">Loading feedback request...</p>
          ) : null}
        </div>

        <div className="px-6 py-4 border-t border-gray-200 flex justify-end gap-2">
          <button
            onClick={handleDismiss}
            disabled={submitting}
            className="px-4 py-2 text-sm rounded-md border border-gray-300 text-gray-700 hover:bg-gray-50 active:scale-95 transition-transform disabled:opacity-50"
          >
            Dismiss
          </button>
          <button
            onClick={handleSubmit}
            disabled={!canSubmit()}
            className="px-4 py-2 text-sm rounded-md bg-indigo-600 text-white hover:bg-indigo-700 hover:brightness-110 active:scale-95 transition-transform disabled:opacity-50"
          >
            {submitting ? "Submitting..." : "Submit"}
          </button>
        </div>
      </div>
    </BaseModal>
  );
}
