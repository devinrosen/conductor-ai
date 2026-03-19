import { useState, useCallback, useRef } from "react";

export interface Toast {
  id: string;
  title: string;
  body: string;
  severity: "info" | "warning" | "action_required";
}

let nextId = 0;

export function useToast() {
  const [toasts, setToasts] = useState<Toast[]>([]);
  const timers = useRef<Map<string, ReturnType<typeof setTimeout>>>(new Map());

  const addToast = useCallback(
    (toast: Omit<Toast, "id">) => {
      const id = `toast-${nextId++}`;
      setToasts((prev) => [...prev, { ...toast, id }]);
      const timer = setTimeout(() => {
        setToasts((prev) => prev.filter((t) => t.id !== id));
        timers.current.delete(id);
      }, 5000);
      timers.current.set(id, timer);
    },
    [],
  );

  const dismissToast = useCallback((id: string) => {
    setToasts((prev) => prev.filter((t) => t.id !== id));
    const timer = timers.current.get(id);
    if (timer) {
      clearTimeout(timer);
      timers.current.delete(id);
    }
  }, []);

  return { toasts, addToast, dismissToast };
}
