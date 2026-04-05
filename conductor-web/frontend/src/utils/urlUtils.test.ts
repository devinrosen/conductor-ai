import { describe, it, expect } from "vitest";
import { isSafeUrl } from "./urlUtils";

describe("isSafeUrl", () => {
  it("allows https: URLs", () => {
    expect(isSafeUrl("https://example.com")).toBe(true);
  });

  it("allows http: URLs", () => {
    expect(isSafeUrl("http://example.com")).toBe(true);
  });

  it("blocks javascript: protocol (XSS vector)", () => {
    expect(isSafeUrl("javascript:alert(1)")).toBe(false);
  });

  it("blocks javascript: with mixed case", () => {
    expect(isSafeUrl("JavaScript:alert(1)")).toBe(false);
  });

  it("blocks data: protocol", () => {
    expect(isSafeUrl("data:text/html,<script>alert(1)</script>")).toBe(false);
  });

  it("blocks vbscript: protocol", () => {
    expect(isSafeUrl("vbscript:msgbox(1)")).toBe(false);
  });

  it("blocks malformed URLs", () => {
    expect(isSafeUrl("not a url")).toBe(false);
  });

  it("blocks empty string", () => {
    expect(isSafeUrl("")).toBe(false);
  });
});
