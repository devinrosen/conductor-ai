/**
 * Extracts a user-friendly error message from an error object or unknown value.
 * @param error - The error object or unknown value
 * @param fallbackMessage - The message to use if the error doesn't have a readable message
 */
export function getErrorMessage(error: unknown, fallbackMessage: string): string {
  // Standard Error objects
  if (error instanceof Error) {
    return error.message;
  }

  // String rejections
  if (typeof error === "string") {
    return error;
  }

  // Objects with message properties (API responses, plain objects, etc.)
  if (error && typeof error === "object") {
    const errorObj = error as Record<string, unknown>;

    // Common error message fields
    if (typeof errorObj.message === "string") {
      return errorObj.message;
    }
    if (typeof errorObj.error === "string") {
      return errorObj.error;
    }
    if (typeof errorObj.detail === "string") {
      return errorObj.detail;
    }

    // API error responses with nested error info
    if (errorObj.error && typeof errorObj.error === "object") {
      const nestedError = errorObj.error as Record<string, unknown>;
      if (typeof nestedError.message === "string") {
        return nestedError.message;
      }
    }

    // Try to stringify if it looks like a structured error
    if (Object.keys(errorObj).length > 0) {
      try {
        const stringified = JSON.stringify(errorObj);
        if (stringified !== "{}") {
          return stringified;
        }
      } catch {
        // Fall through to fallback
      }
    }
  }

  // Fallback for all other types (undefined, null, numbers, etc.)
  return fallbackMessage;
}