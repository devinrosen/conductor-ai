import { useApi } from "./useApi";
import { api } from "../api/client";

/** Fetches the canonical terminal Vantage conductor statuses from the backend. */
export function useVantageTerminalStatuses() {
  return useApi(() => api.getVantageTerminalStatuses(), []);
}
