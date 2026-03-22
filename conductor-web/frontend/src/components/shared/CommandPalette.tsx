import { useState, useEffect, useCallback, useRef, useMemo } from "react";
import { useNavigate } from "react-router";
import { useRepos } from "../layout/AppShell";

interface CommandItem {
  id: string;
  label: string;
  section: string;
  action: () => void;
  keywords?: string;
}

function fuzzyMatch(query: string, text: string): boolean {
  const q = query.toLowerCase();
  const t = text.toLowerCase();
  let qi = 0;
  for (let ti = 0; ti < t.length && qi < q.length; ti++) {
    if (t[ti] === q[qi]) qi++;
  }
  return qi === q.length;
}

interface CommandPaletteProps {
  open: boolean;
  onClose: () => void;
}

export function CommandPalette({ open, onClose }: CommandPaletteProps) {
  const [query, setQuery] = useState("");
  const [selectedIndex, setSelectedIndex] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const navigate = useNavigate();
  const { repos } = useRepos();

  const go = useCallback(
    (path: string) => {
      onClose();
      navigate(path);
    },
    [navigate, onClose],
  );

  const commands = useMemo<CommandItem[]>(() => {
    const items: CommandItem[] = [
      { id: "nav-activity", label: "Activity", section: "Navigation", action: () => go("/"), keywords: "home dashboard" },
      { id: "nav-repos", label: "Repos", section: "Navigation", action: () => go("/repos"), keywords: "repositories stations" },
      { id: "nav-workflows", label: "Workflows", section: "Navigation", action: () => go("/workflows"), keywords: "timetable runs" },
      { id: "nav-tickets", label: "Tickets", section: "Navigation", action: () => go("/tickets"), keywords: "issues" },
      { id: "nav-settings", label: "Settings", section: "Navigation", action: () => go("/settings"), keywords: "config preferences" },
    ];

    for (const repo of repos) {
      items.push({
        id: `repo-${repo.id}`,
        label: repo.slug,
        section: "Repos",
        action: () => go(`/repos/${repo.id}`),
        keywords: repo.remote_url,
      });
    }

    return items;
  }, [repos, go]);

  const filtered = useMemo(() => {
    if (!query) return commands;
    return commands.filter(
      (c) =>
        fuzzyMatch(query, c.label) ||
        fuzzyMatch(query, c.section) ||
        (c.keywords && fuzzyMatch(query, c.keywords)),
    );
  }, [commands, query]);

  // Reset selection when results change
  useEffect(() => {
    setSelectedIndex(0);
  }, [filtered.length]);

  // Focus input when opened
  useEffect(() => {
    if (open) {
      setQuery("");
      setSelectedIndex(0);
      requestAnimationFrame(() => inputRef.current?.focus());
    }
  }, [open]);

  // Scroll selected item into view
  useEffect(() => {
    const el = listRef.current?.querySelector(`[data-index="${selectedIndex}"]`);
    el?.scrollIntoView({ block: "nearest" });
  }, [selectedIndex]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      switch (e.key) {
        case "ArrowDown":
          e.preventDefault();
          setSelectedIndex((i) => Math.min(i + 1, filtered.length - 1));
          break;
        case "ArrowUp":
          e.preventDefault();
          setSelectedIndex((i) => Math.max(i - 1, 0));
          break;
        case "Enter":
          e.preventDefault();
          filtered[selectedIndex]?.action();
          break;
        case "Escape":
          e.preventDefault();
          onClose();
          break;
      }
    },
    [filtered, selectedIndex, onClose],
  );

  if (!open) return null;

  // Group filtered items by section
  const sections = new Map<string, CommandItem[]>();
  for (const item of filtered) {
    const list = sections.get(item.section) ?? [];
    list.push(item);
    sections.set(item.section, list);
  }

  let flatIndex = 0;

  return (
    <div className="fixed inset-0 z-50 flex items-start justify-center pt-[15vh]" onClick={onClose}>
      <div className="fixed inset-0 bg-black/50" />
      <div
        className="relative w-full max-w-lg rounded-lg border border-gray-200 bg-white shadow-2xl overflow-hidden"
        onClick={(e) => e.stopPropagation()}
        onKeyDown={handleKeyDown}
      >
        <div className="flex items-center border-b border-gray-200 px-3">
          <span className="text-gray-400 text-sm mr-2">&#8984;</span>
          <input
            ref={inputRef}
            type="text"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Search commands..."
            className="flex-1 py-2.5 text-sm bg-transparent outline-none text-gray-900 placeholder-gray-400"
          />
          <kbd className="text-[10px] text-gray-400 font-mono border border-gray-200 rounded px-1 py-0.5">
            esc
          </kbd>
        </div>
        <div ref={listRef} className="max-h-72 overflow-y-auto py-1">
          {filtered.length === 0 && (
            <div className="px-3 py-6 text-center text-sm text-gray-400">
              No results found
            </div>
          )}
          {Array.from(sections.entries()).map(([section, items]) => (
            <div key={section}>
              <div className="px-3 pt-2 pb-1 text-[10px] font-semibold uppercase tracking-wider text-gray-400">
                {section}
              </div>
              {items.map((item) => {
                const idx = flatIndex++;
                return (
                  <button
                    key={item.id}
                    data-index={idx}
                    onClick={() => item.action()}
                    className={`w-full text-left px-3 py-1.5 text-sm flex items-center gap-2 ${
                      idx === selectedIndex
                        ? "bg-indigo-100 text-indigo-700"
                        : "text-gray-700 hover:bg-gray-100"
                    }`}
                  >
                    {item.label}
                  </button>
                );
              })}
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}
