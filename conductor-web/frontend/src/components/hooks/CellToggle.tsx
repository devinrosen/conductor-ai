/** Cell state for the hook × event matrix. */
export type CellMode = "off" | "any" | "root";

interface CellToggleProps {
  /** Current cell state. */
  mode: CellMode;
  /** Whether this event supports the "root" option (workflow events only). */
  isWorkflow: boolean;
  /** Disable interaction while a PATCH is in-flight. */
  disabled?: boolean;
  onChange: (mode: CellMode) => void;
}

const BASE =
  "px-1.5 py-0.5 text-[10px] font-semibold leading-tight rounded cursor-pointer select-none transition-colors";
const DISABLED = "opacity-40 cursor-not-allowed";

/**
 * Compact segmented toggle for a single matrix cell.
 *
 * - **2-state** (non-workflow): Off / On
 * - **3-state** (workflow): Off / Any / Root
 */
export function CellToggle({ mode, isWorkflow, disabled, onChange }: CellToggleProps) {
  function click(next: CellMode) {
    if (disabled) return;
    // Clicking the active segment turns it off; clicking another activates it.
    onChange(mode === next ? "off" : next);
  }

  return (
    <span
      className={`inline-flex rounded border border-gray-200 dark:border-gray-600 overflow-hidden ${disabled ? DISABLED : ""}`}
    >
      {/* Any / On */}
      <button
        type="button"
        onClick={() => click("any")}
        disabled={disabled}
        title={isWorkflow ? "Fire on any workflow (root + child)" : "Enable"}
        className={`${BASE} ${
          mode === "any"
            ? "bg-blue-600 text-white"
            : "bg-gray-100 text-gray-400 hover:bg-gray-200 dark:bg-gray-700 dark:text-gray-400 dark:hover:bg-gray-600"
        }`}
      >
        {isWorkflow ? "Any" : "On"}
      </button>

      {/* Root (workflow events only) */}
      {isWorkflow && (
        <button
          type="button"
          onClick={() => click("root")}
          disabled={disabled}
          title="Fire only on root workflows (skip child/nested)"
          className={`${BASE} border-l border-gray-200 dark:border-gray-600 ${
            mode === "root"
              ? "bg-amber-500 text-white"
              : "bg-gray-100 text-gray-400 hover:bg-gray-200 dark:bg-gray-700 dark:text-gray-400 dark:hover:bg-gray-600"
          }`}
        >
          Root
        </button>
      )}
    </span>
  );
}
