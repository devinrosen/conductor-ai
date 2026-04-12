import { defineConfig, devices } from "@playwright/test";
import { E2E_CONDUCTOR_HOME } from "./e2e/e2e-db-path";

export default defineConfig({
  testDir: "./e2e",
  globalTeardown: "./e2e/global-teardown",
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI
    ? [["html", { open: "never" }], ["list"]]
    : "list",
  use: {
    baseURL: "http://localhost:3000",
  },
  projects: [
    {
      name: "iPhone 14",
      use: { ...devices["iPhone 14"] },
    },
    {
      name: "Pixel 7",
      use: { ...devices["Pixel 7"] },
    },
  ],
  webServer: {
    command: "../../target/debug/conductor-web",
    url: "http://localhost:3000",
    reuseExistingServer: !process.env.CI,
    timeout: 30_000,
    env: { CONDUCTOR_HOME: E2E_CONDUCTOR_HOME },
  },
});
