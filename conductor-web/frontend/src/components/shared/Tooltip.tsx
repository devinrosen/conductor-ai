import { useState, useRef, useCallback, useLayoutEffect, useId, type ReactNode } from "react";
import { createPortal } from "react-dom";

interface TooltipProps {
  content: string;
  children: ReactNode;
  /** Delay in ms before showing. Default: 200 */
  delay?: number;
}

export function Tooltip({ content, children, delay = 200 }: TooltipProps) {
  const [visible, setVisible] = useState(false);
  const [coords, setCoords] = useState<{ top: number; left: number } | null>(null);
  const triggerRef = useRef<HTMLSpanElement>(null);
  const timeout = useRef<ReturnType<typeof setTimeout> | null>(null);
  const tooltipId = useId();

  const show = useCallback(() => {
    timeout.current = setTimeout(() => setVisible(true), delay);
  }, [delay]);

  const hide = useCallback(() => {
    if (timeout.current) clearTimeout(timeout.current);
    setVisible(false);
  }, []);

  // Measure trigger position after becoming visible
  useLayoutEffect(() => {
    if (!visible || !triggerRef.current) return;
    const rect = triggerRef.current.getBoundingClientRect();
    setCoords({
      top: rect.top - 4,
      left: rect.left + rect.width / 2,
    });
  }, [visible]);

  return (
    <span
      ref={triggerRef}
      className="inline-flex"
      onMouseEnter={show}
      onMouseLeave={hide}
      onFocus={show}
      onBlur={hide}
      aria-describedby={visible ? tooltipId : undefined}
    >
      {children}
      {visible && coords && createPortal(
        <span
          id={tooltipId}
          role="tooltip"
          className="fixed z-[9999] pointer-events-none"
          style={{ top: coords.top, left: coords.left, transform: "translate(-50%, -100%)" }}
        >
          <span className="block whitespace-nowrap rounded px-2 py-1 text-[11px] text-white bg-gray-800 shadow-lg">
            {content}
          </span>
          <span className="absolute top-full left-1/2 -translate-x-1/2 w-0 h-0 border-l-4 border-r-4 border-t-4 border-l-transparent border-r-transparent border-t-gray-800" />
        </span>,
        document.body,
      )}
    </span>
  );
}
