import * as crypto from "crypto";
import * as os from "os";
import * as path from "path";
import * as fs from "fs";

export const E2E_CONDUCTOR_HOME = path.join(
  os.tmpdir(),
  `conductor-e2e-${crypto.randomBytes(6).toString("hex")}`
);

// Pre-create the directory so open_database() doesn't fail before ensure_dirs() runs.
fs.mkdirSync(E2E_CONDUCTOR_HOME, { recursive: true });
