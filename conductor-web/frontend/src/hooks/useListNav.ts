import { useState, useCallback, useEffect } from "react";

export function useListNav(itemCount: number) {
  const [selectedIndex, setSelectedIndex] = useState(-1);

  // Clamp selection when list shrinks
  useEffect(() => {
    if (selectedIndex >= itemCount) {
      setSelectedIndex(itemCount > 0 ? itemCount - 1 : -1);
    }
  }, [itemCount, selectedIndex]);

  // Scroll selected row into view
  useEffect(() => {
    if (selectedIndex < 0) return;
    const row = document.querySelector(`[data-list-index="${selectedIndex}"]`);
    row?.scrollIntoView({ block: "nearest", behavior: "smooth" });
  }, [selectedIndex]);

  const moveDown = useCallback(() => {
    setSelectedIndex((prev) =>
      prev < itemCount - 1 ? prev + 1 : prev === -1 ? 0 : prev,
    );
  }, [itemCount]);

  const moveUp = useCallback(() => {
    setSelectedIndex((prev) => (prev > 0 ? prev - 1 : prev));
  }, []);

  const reset = useCallback(() => setSelectedIndex(-1), []);

  return { selectedIndex, setSelectedIndex, moveDown, moveUp, reset };
}
