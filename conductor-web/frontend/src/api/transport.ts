/**
 * Transport adapter for conductor frontend.
 *
 * Detects whether the app is running inside Tauri (desktop) or in a browser
 * (web) and provides the appropriate transport for API calls.
 *
 * - **Web mode**: Uses `fetch()` against the REST API (default behavior)
 * - **Tauri mode**: Uses `window.__TAURI_INTERNALS__.invoke()` to call Rust commands directly
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

// Tauri v2 injects this global at startup.
declare global {
  interface Window {
    __TAURI_INTERNALS__?: {
      invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
    };
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

/**
 * Invoke a Tauri command by name.
 *
 * Uses the injected `window.__TAURI_INTERNALS__.invoke()` directly,
 * avoiding any build-time dependency on `@tauri-apps/api`.
 *
 * In web mode, this function throws — callers should check `isDesktop()` first
 * or use the higher-level API functions in `client.ts` which handle both modes.
 */
export async function invokeCommand<T>(
  command: string,
  args?: Record<string, unknown>,
): Promise<T> {
  const internals = window.__TAURI_INTERNALS__;
  if (!internals) {
    throw new Error(
      `invokeCommand('${command}') called outside Tauri context`,
    );
  }
  return internals.invoke(command, args) as Promise<T>;
}
