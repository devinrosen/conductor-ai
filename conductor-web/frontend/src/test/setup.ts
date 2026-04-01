import { expect } from "vitest";
import * as matchers from "@testing-library/jest-dom/matchers";

// Extend vitest matchers with jest-dom
expect.extend(matchers);