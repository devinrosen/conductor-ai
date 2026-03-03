import { useState, useEffect } from "react";
import { api } from "../../api/client";
import type { KnownModel } from "../../api/types";

const TIER_STYLES: Record<number, string> = {
  3: "text-purple-600",
  2: "text-blue-600",
  1: "text-green-600",
};

const TIER_STARS: Record<number, string> = {
  3: "\u2605\u2605\u2605",
  2: "\u2605\u2605",
  1: "\u2605",
};

interface ModelPickerProps {
  /** Currently selected model (id or alias), or null if not set */
  value: string | null;
  /** Called when the user selects a model (alias or custom string), or null to clear */
  onChange: (model: string | null) => void;
  /** The effective default model from the resolution chain */
  effectiveDefault?: string | null;
  /** Where the effective default comes from (e.g. "global config", "repo") */
  effectiveSource?: string;
  /** Suggested model alias from prompt analysis */
  suggested?: string | null;
  /** Whether the picker is disabled */
  disabled?: boolean;
}

export function ModelPicker({
  value,
  onChange,
  effectiveDefault,
  effectiveSource,
  suggested,
  disabled,
}: ModelPickerProps) {
  const [models, setModels] = useState<KnownModel[]>([]);
  const [showCustom, setShowCustom] = useState(false);
  const [customInput, setCustomInput] = useState("");

  useEffect(() => {
    api.listKnownModels().then(setModels).catch(() => {});
  }, []);

  const isCustomValue =
    value !== null && !models.some((m) => m.alias === value || m.id === value);

  return (
    <div className="space-y-2">
      {/* Effective default display */}
      {effectiveDefault !== undefined && (
        <div className="text-xs text-gray-500">
          Using:{" "}
          <span className="font-mono font-medium text-gray-700">
            {effectiveDefault ?? "claude default"}
          </span>
          {effectiveSource && (
            <span className="text-gray-400"> (from {effectiveSource})</span>
          )}
        </div>
      )}

      {/* Model options */}
      <div className="rounded-lg border border-gray-200 bg-white overflow-hidden">
        {models.map((model) => {
          const isSelected = value === model.alias || value === model.id;
          const isCurrent =
            effectiveDefault === model.alias || effectiveDefault === model.id;
          const isSuggested = suggested === model.alias && !isCurrent;

          return (
            <button
              key={model.id}
              type="button"
              disabled={disabled}
              onClick={() => onChange(model.alias)}
              className={`w-full flex items-center gap-3 px-3 py-2 text-sm text-left transition-colors ${
                isSelected
                  ? "bg-indigo-50 border-l-2 border-indigo-500"
                  : "border-l-2 border-transparent hover:bg-gray-50"
              } ${disabled ? "opacity-50 cursor-not-allowed" : "cursor-pointer"}`}
            >
              <span
                className={`text-xs w-10 shrink-0 ${TIER_STYLES[model.tier] ?? "text-gray-400"}`}
              >
                {TIER_STARS[model.tier] ?? ""}
              </span>
              <span className="font-mono font-medium w-16 shrink-0">
                {model.alias}
              </span>
              <span className="text-gray-500 text-xs flex-1">
                {model.description}
              </span>
              <span className="flex items-center gap-1 shrink-0">
                {isCurrent && (
                  <span className="text-xs text-gray-400">(current)</span>
                )}
                {isSuggested && (
                  <span className="inline-flex items-center px-1.5 py-0.5 rounded text-xs font-medium bg-green-100 text-green-700">
                    Suggested
                  </span>
                )}
                {isSelected && (
                  <span className="text-indigo-600 text-xs font-medium">
                    &#10003;
                  </span>
                )}
              </span>
            </button>
          );
        })}

        {/* Custom option */}
        {showCustom || isCustomValue ? (
          <div
            className={`flex items-center gap-2 px-3 py-2 border-l-2 ${
              isCustomValue
                ? "bg-indigo-50 border-indigo-500"
                : "border-transparent"
            }`}
          >
            <span className="text-xs text-gray-400 w-10 shrink-0">
              &bull;&bull;&bull;
            </span>
            <input
              type="text"
              value={isCustomValue ? value ?? "" : customInput}
              onChange={(e) => {
                setCustomInput(e.target.value);
                if (e.target.value.trim()) {
                  onChange(e.target.value.trim());
                }
              }}
              onKeyDown={(e) => {
                if (e.key === "Escape") {
                  setShowCustom(false);
                  setCustomInput("");
                }
              }}
              placeholder="custom model ID..."
              className="flex-1 text-sm font-mono border border-gray-300 rounded px-2 py-0.5 focus:outline-none focus:ring-1 focus:ring-indigo-500"
              autoFocus={!isCustomValue}
              disabled={disabled}
            />
            <button
              type="button"
              onClick={() => {
                setShowCustom(false);
                setCustomInput("");
              }}
              className="text-xs text-gray-400 hover:text-gray-600"
            >
              &times;
            </button>
          </div>
        ) : (
          <button
            type="button"
            disabled={disabled}
            onClick={() => setShowCustom(true)}
            className={`w-full flex items-center gap-3 px-3 py-2 text-sm text-left border-l-2 border-transparent hover:bg-gray-50 text-gray-400 ${disabled ? "opacity-50 cursor-not-allowed" : "cursor-pointer"}`}
          >
            <span className="text-xs w-10 shrink-0">&bull;&bull;&bull;</span>
            <span className="font-mono">custom&hellip;</span>
          </button>
        )}
      </div>

      {/* Clear button */}
      {value && (
        <button
          type="button"
          disabled={disabled}
          onClick={() => {
            onChange(null);
            setShowCustom(false);
            setCustomInput("");
          }}
          className="text-xs text-red-600 hover:text-red-700"
        >
          Clear model override
        </button>
      )}
    </div>
  );
}
