import { useState, useEffect, useCallback, useMemo, useRef } from "react";
import { api } from "../../api/client";
import type { Notification } from "../../api/types";
import {
  useConductorEvents,
  type ConductorEventType,
  type ConductorEventData,
} from "../../hooks/useConductorEvents";
import { NotificationPanel } from "./NotificationPanel";

export function NotificationBell() {
  const [open, setOpen] = useState(false);
  const [notifications, setNotifications] = useState<Notification[]>([]);
  const [unreadCount, setUnreadCount] = useState(0);
  const bellRef = useRef<HTMLDivElement>(null);

  const fetchNotifications = useCallback(async () => {
    try {
      const [items, { count }] = await Promise.all([
        api.listNotifications(false, 30),
        api.unreadNotificationCount(),
      ]);
      setNotifications(items);
      setUnreadCount(count);
    } catch {
      // Silently ignore — bell will show stale data
    }
  }, []);

  useEffect(() => {
    fetchNotifications();
  }, [fetchNotifications]);

  // Refresh on SSE notification_created events
  const handlers = useMemo((): Partial<
    Record<ConductorEventType, (data: ConductorEventData) => void>
  > => ({
    notification_created: () => fetchNotifications(),
  }), [fetchNotifications]);

  useConductorEvents(handlers);

  // Close panel on outside click
  useEffect(() => {
    if (!open) return;
    const handleClick = (e: MouseEvent) => {
      if (bellRef.current && !bellRef.current.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, [open]);

  const handleMarkRead = useCallback(
    async (id: string) => {
      try {
        await api.markNotificationRead(id);
        fetchNotifications();
      } catch {
        // ignore
      }
    },
    [fetchNotifications],
  );

  const handleMarkAllRead = useCallback(async () => {
    try {
      await api.markAllNotificationsRead();
      fetchNotifications();
    } catch {
      // ignore
    }
  }, [fetchNotifications]);

  return (
    <div ref={bellRef} className="relative">
      <button
        onClick={() => setOpen(!open)}
        className="relative p-2 text-gray-600 hover:text-gray-900 hover:bg-gray-100 rounded-md"
        aria-label="Notifications"
      >
        <svg
          xmlns="http://www.w3.org/2000/svg"
          className="h-5 w-5"
          fill="none"
          viewBox="0 0 24 24"
          stroke="currentColor"
          strokeWidth={2}
        >
          <path
            strokeLinecap="round"
            strokeLinejoin="round"
            d="M15 17h5l-1.405-1.405A2.032 2.032 0 0118 14.158V11a6.002 6.002 0 00-4-5.659V5a2 2 0 10-4 0v.341C7.67 6.165 6 8.388 6 11v3.159c0 .538-.214 1.055-.595 1.436L4 17h5m6 0v1a3 3 0 11-6 0v-1m6 0H9"
          />
        </svg>
        {unreadCount > 0 && (
          <span className="absolute -top-0.5 -right-0.5 flex h-4 min-w-4 items-center justify-center rounded-full bg-red-500 px-1 text-[10px] font-bold text-white">
            {unreadCount > 99 ? "99+" : unreadCount}
          </span>
        )}
      </button>
      {open && (
        <NotificationPanel
          notifications={notifications}
          onMarkRead={handleMarkRead}
          onMarkAllRead={handleMarkAllRead}
          onClose={() => setOpen(false)}
        />
      )}
    </div>
  );
}
