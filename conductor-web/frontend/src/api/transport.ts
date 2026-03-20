/**
 * Transport adapter for conductor frontend.
 *
 * Detects whether the app is running inside Tauri (desktop) or in a browser
 * (web) and provides the appropriate transport for API calls.
 *
 * - **Web mode**: Uses `fetch()` against the REST API (default behavior)
 * - **Tauri mode**: Uses `@tauri-apps/api/core` invoke to call Rust commands directly
 *
 * Usage:
 *   import { isDesktop, invokeCommand } from './transport';
 *
 *   if (isDesktop()) {
 *     const repos = await invokeCommand<Repo[]>('list_repos');
 *   } else {
 *     const repos = await request<Repo[]>('/repos');
 *   }
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
 *
 * In web mode, this function throws — callers should check `isDesktop()` first
 * or use the higher-level API functions in `client.ts` which handle both modes.
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
