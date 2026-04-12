import * as fs from "fs";
import { E2E_CONDUCTOR_HOME } from "./e2e-db-path";

export default async function globalTeardown() {
  fs.rmSync(E2E_CONDUCTOR_HOME, { recursive: true, force: true });
}
