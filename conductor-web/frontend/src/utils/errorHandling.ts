/**
 * Extracts a user-friendly error message from an error object or unknown value.
 * @param error - The error object or unknown value
 * @param fallbackMessage - The message to use if the error doesn't have a readable message
 */
export function getErrorMessage(error: unknown, fallbackMessage: string): string {
  return error instanceof Error ? error.message : fallbackMessage;
}