import { useEffect } from "react";

interface ShortcutHelpProps {
  open: boolean;
  onClose: () => void;
}

interface ShortcutEntry {
  keys: string[];
  description: string;
}

interface ShortcutGroup {
  title: string;
  shortcuts: ShortcutEntry[];
}

const groups: ShortcutGroup[] = [
  {
    title: "Navigation",
    shortcuts: [
      { keys: ["g", "d"], description: "Go to Dashboard" },
      { keys: ["g", "t"], description: "Go to Tickets" },
      { keys: ["g", "s"], description: "Go to Settings" },
    ],
  },
  {
    title: "Lists",
    shortcuts: [
      { keys: ["j"], description: "Move down" },
      { keys: ["k"], description: "Move up" },
      { keys: ["Enter"], description: "Open selected" },
    ],
  },
  {
    title: "Actions",
    shortcuts: [
      { keys: ["c"], description: "Create (repo / worktree)" },
      { keys: ["d"], description: "Delete (with confirmation)" },
      { keys: ["/"], description: "Focus search" },
    ],
  },
  {
    title: "General",
    shortcuts: [
      { keys: ["?"], description: "Show this help" },
      { keys: ["Esc"], description: "Close modal / clear" },
    ],
  },
];

function Kbd({ children }: { children: string }) {
  return (
    <kbd className="inline-flex items-center justify-center min-w-[1.5rem] px-1.5 py-0.5 text-xs font-mono font-medium bg-gray-100 border border-gray-300 rounded text-gray-700">
      {children}
    </kbd>
  );
}

export function KeyboardShortcutHelp({ open, onClose }: ShortcutHelpProps) {
  useEffect(() => {
    if (!open) return;
    function handleKeyDown(e: KeyboardEvent) {
      if (e.key === "Escape") onClose();
    }
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [open, onClose]);

  if (!open) return null;

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/40"
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div className="bg-white rounded-lg shadow-lg w-full max-w-md mx-4 p-6">
        <div className="flex items-center justify-between mb-4">
          <h3 className="text-lg font-semibold text-gray-900">
            Keyboard Shortcuts
          </h3>
          <button
            onClick={onClose}
            className="text-gray-400 hover:text-gray-600 text-xl leading-none"
          >
            &times;
          </button>
        </div>

        <div className="space-y-4">
          {groups.map((group) => (
            <div key={group.title}>
              <h4 className="text-xs font-semibold uppercase tracking-wider text-gray-400 mb-2">
                {group.title}
              </h4>
              <div className="space-y-1.5">
                {group.shortcuts.map((shortcut) => (
                  <div
                    key={shortcut.description}
                    className="flex items-center justify-between text-sm"
                  >
                    <span className="text-gray-600">
                      {shortcut.description}
                    </span>
                    <span className="flex gap-1">
                      {shortcut.keys.map((k, i) => (
                        <span key={i} className="flex items-center gap-0.5">
                          {i > 0 && (
                            <span className="text-gray-400 text-xs mx-0.5">
                              then
                            </span>
                          )}
                          <Kbd>{k}</Kbd>
                        </span>
                      ))}
                    </span>
                  </div>
                ))}
              </div>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}
