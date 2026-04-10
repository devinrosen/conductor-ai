import { describe, it, expect } from "vitest";
import { getErrorMessage } from "./errorHandling";

describe("getErrorMessage", () => {
  const fallbackMessage = "Something went wrong";

  it("extracts message from standard Error objects", () => {
    const error = new Error("Network connection failed");
    expect(getErrorMessage(error, fallbackMessage)).toBe("Network connection failed");
  });

  it("returns string rejections directly", () => {
    const error = "API returned 404";
    expect(getErrorMessage(error, fallbackMessage)).toBe("API returned 404");
  });

  it("extracts message property from objects", () => {
    const error = { message: "Validation failed" };
    expect(getErrorMessage(error, fallbackMessage)).toBe("Validation failed");
  });

  it("extracts error property from objects", () => {
    const error = { error: "Unauthorized access" };
    expect(getErrorMessage(error, fallbackMessage)).toBe("Unauthorized access");
  });

  it("extracts detail property from objects", () => {
    const error = { detail: "Missing required field" };
    expect(getErrorMessage(error, fallbackMessage)).toBe("Missing required field");
  });

  it("extracts nested error message", () => {
    const error = { error: { message: "Database connection lost" } };
    expect(getErrorMessage(error, fallbackMessage)).toBe("Database connection lost");
  });

  it("prefers direct message over nested error message", () => {
    const error = {
      message: "Direct message",
      error: { message: "Nested message" }
    };
    expect(getErrorMessage(error, fallbackMessage)).toBe("Direct message");
  });

  it("uses fallback for objects without known error fields", () => {
    const error = { status: 500, headers: {}, body: {} };
    expect(getErrorMessage(error, fallbackMessage)).toBe(fallbackMessage);
  });

  it("uses fallback for empty objects", () => {
    const error = {};
    expect(getErrorMessage(error, fallbackMessage)).toBe(fallbackMessage);
  });

  it("uses fallback for null", () => {
    expect(getErrorMessage(null, fallbackMessage)).toBe(fallbackMessage);
  });

  it("uses fallback for undefined", () => {
    expect(getErrorMessage(undefined, fallbackMessage)).toBe(fallbackMessage);
  });

  it("uses fallback for numbers", () => {
    expect(getErrorMessage(404, fallbackMessage)).toBe(fallbackMessage);
  });

  it("uses fallback for booleans", () => {
    expect(getErrorMessage(false, fallbackMessage)).toBe(fallbackMessage);
  });

  it("handles non-string message properties gracefully", () => {
    const error = { message: 42 };
    expect(getErrorMessage(error, fallbackMessage)).toBe(fallbackMessage);
  });

  it("handles non-string error properties gracefully", () => {
    const error = { error: 404 };
    expect(getErrorMessage(error, fallbackMessage)).toBe(fallbackMessage);
  });

  it("handles complex nested structures without leaking internal data", () => {
    const error = {
      status: 403,
      headers: { "content-type": "application/json" },
      body: { internalCode: "AUTH_001", stack: "...", config: "..." }
    };
    expect(getErrorMessage(error, fallbackMessage)).toBe(fallbackMessage);
  });
});