import { useState, useRef, useCallback, type ReactNode } from "react";

interface TooltipProps {
  content: string;
  children: ReactNode;
  /** Placement relative to the trigger. Default: "top" */
  placement?: "top" | "bottom";
  /** Delay in ms before showing. Default: 200 */
  delay?: number;
}

export function Tooltip({ content, children, placement = "top", delay = 200 }: TooltipProps) {
  const [visible, setVisible] = useState(false);
  const timeout = useRef<ReturnType<typeof setTimeout> | null>(null);

  const show = useCallback(() => {
    timeout.current = setTimeout(() => setVisible(true), delay);
  }, [delay]);

  const hide = useCallback(() => {
    if (timeout.current) clearTimeout(timeout.current);
    setVisible(false);
  }, []);

  const posClass = placement === "top"
    ? "bottom-full left-1/2 -translate-x-1/2 mb-1.5"
    : "top-full left-1/2 -translate-x-1/2 mt-1.5";

  const arrowClass = placement === "top"
    ? "top-full left-1/2 -translate-x-1/2 border-t-gray-800"
    : "bottom-full left-1/2 -translate-x-1/2 border-b-gray-800";

  const arrowBorder = placement === "top"
    ? "border-l-transparent border-r-transparent border-b-transparent border-t-4 border-x-4 border-b-0"
    : "border-l-transparent border-r-transparent border-t-transparent border-b-4 border-x-4 border-t-0";

  return (
    <span className="relative inline-flex" onMouseEnter={show} onMouseLeave={hide} onFocus={show} onBlur={hide}>
      {children}
      {visible && (
        <span className={`absolute ${posClass} z-50 pointer-events-none`}>
          <span className="block whitespace-nowrap rounded px-2 py-1 text-[11px] text-white bg-gray-800 shadow-lg">
            {content}
          </span>
          <span className={`absolute ${arrowClass} w-0 h-0 ${arrowBorder}`} />
        </span>
      )}
    </span>
  );
}
