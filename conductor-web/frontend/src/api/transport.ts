/**
 * Transport adapter for conductor frontend.
 *
 * Detects whether the app is running inside Tauri (desktop) or in a browser
 * (web) and provides the appropriate base URL for API calls.
 *
 * - **Web mode**: Uses relative `/api` paths (served by the same origin)
 * - **Tauri mode**: Queries the embedded server port via `get_api_port` and
 *   uses `http://127.0.0.1:{port}/api` as the base URL.
 */

// Tauri v2 injects this global at startup; we use it only for detection.
declare global {
  interface Window {
    __TAURI_INTERNALS__?: unknown;
  }
}

/**
 * Check if the app is running inside a Tauri desktop container.
 */
export function isDesktop(): boolean {
  return (
    typeof window !== "undefined" &&
    window.__TAURI_INTERNALS__ !== undefined
  );
}

// Lazily cached Tauri invoke function to avoid repeated dynamic imports.
let cachedInvoke: typeof import("@tauri-apps/api/core").invoke | null = null;

/**
 * Invoke a Tauri command by name.
 *
 * Uses a lazy dynamic import of `@tauri-apps/api/core` so the Tauri SDK is
 * only bundled when running inside the desktop container (tree-shaken in
 * web-only builds).
 */
export async function invokeCommand<T>(
  command: string,
  args?: Record<string, unknown>,
): Promise<T> {
  if (!cachedInvoke) {
    const mod = await import("@tauri-apps/api/core");
    cachedInvoke = mod.invoke;
  }
  return cachedInvoke<T>(command, args);
}

// Cached base URL for the API — resolved once, reused for all requests.
let cachedBaseUrl: string | null = null;

/**
 * Returns the base URL for API requests.
 *
 * - Web mode: `/api` (relative, same origin)
 * - Desktop mode: `http://127.0.0.1:{port}/api` (embedded server)
 */
export async function getApiBaseUrl(): Promise<string> {
  if (cachedBaseUrl !== null) return cachedBaseUrl;

  if (isDesktop()) {
    const port = await invokeCommand<number>("get_api_port");
    cachedBaseUrl = `http://127.0.0.1:${port}/api`;
  } else {
    cachedBaseUrl = "/api";
  }
  return cachedBaseUrl;
}

/**
 * Returns the base origin for non-API connections (e.g. EventSource).
 *
 * - Web mode: empty string (relative URLs work)
 * - Desktop mode: `http://127.0.0.1:{port}`
 */
export async function getApiOrigin(): Promise<string> {
  if (isDesktop()) {
    const port = await invokeCommand<number>("get_api_port");
    return `http://127.0.0.1:${port}`;
  }
  return "";
}
