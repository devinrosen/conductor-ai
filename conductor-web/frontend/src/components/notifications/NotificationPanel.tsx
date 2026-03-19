import type { Notification } from "../../api/types";

function timeAgo(dateStr: string): string {
  const now = Date.now();
  const then = new Date(dateStr).getTime();
  const diff = Math.max(0, now - then);
  const mins = Math.floor(diff / 60000);
  if (mins < 1) return "<1m ago";
  if (mins < 60) return `${mins}m ago`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.floor(hours / 24);
  return `${days}d ago`;
}

function severityIcon(severity: string): string {
  switch (severity) {
    case "action_required":
      return "\u{1F534}";
    case "warning":
      return "\u{1F7E1}";
    default:
      return "\u{1F535}";
  }
}

interface NotificationPanelProps {
  notifications: Notification[];
  onMarkRead: (id: string) => void;
  onMarkAllRead: () => void;
  onClose: () => void;
}

export function NotificationPanel({
  notifications,
  onMarkRead,
  onMarkAllRead,
  onClose,
}: NotificationPanelProps) {
  const unreadCount = notifications.filter((n) => !n.read).length;

  return (
    <div className="absolute right-0 top-full mt-1 w-96 max-h-[28rem] bg-white border border-gray-200 rounded-lg shadow-lg z-50 flex flex-col">
      <div className="flex items-center justify-between px-4 py-2 border-b border-gray-100">
        <span className="font-semibold text-sm text-gray-900">
          Notifications
        </span>
        {unreadCount > 0 && (
          <button
            onClick={onMarkAllRead}
            className="text-xs text-blue-600 hover:text-blue-800"
          >
            Mark all read
          </button>
        )}
      </div>
      <div className="overflow-y-auto flex-1">
        {notifications.length === 0 ? (
          <div className="px-4 py-8 text-center text-sm text-gray-400">
            No notifications
          </div>
        ) : (
          notifications.map((n) => (
            <div
              key={n.id}
              className={`px-4 py-3 border-b border-gray-50 hover:bg-gray-50 cursor-pointer ${
                !n.read ? "bg-blue-50/40" : ""
              }`}
              onClick={() => {
                if (!n.read) onMarkRead(n.id);
                onClose();
              }}
            >
              <div className="flex items-start gap-2">
                <span className="text-sm mt-0.5">{severityIcon(n.severity)}</span>
                <div className="flex-1 min-w-0">
                  <div className="flex items-center justify-between">
                    <span
                      className={`text-sm truncate ${
                        !n.read ? "font-semibold text-gray-900" : "text-gray-700"
                      }`}
                    >
                      {n.title}
                    </span>
                    <span className="text-xs text-gray-400 ml-2 whitespace-nowrap">
                      {timeAgo(n.created_at)}
                    </span>
                  </div>
                  <p className="text-xs text-gray-500 truncate mt-0.5">
                    {n.body}
                  </p>
                </div>
              </div>
            </div>
          ))
        )}
      </div>
    </div>
  );
}
