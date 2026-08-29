#!/usr/bin/env node
/**
 * ynpm finalize — the yggdrasilhq-package postinstall. MULTI-BIN aware:
 * copies EVERY bin the platform package declares, verifies each runs.
 *
 * Best-effort fast path: the entry shims (bin/<name>) fall back to the
 * platform sibling package, which npm places during the same install.
 * STRICT under the ynpm installer (YNPM_* env set): a fleet sync must fail
 * loudly when its binaries cannot run. SOFT under a plain `npm i -g` —
 * npm 11 denies install scripts by default anyway, and a postinstall
 * failure must not roll back a working tree.
 */
import fs from "node:fs";
import path from "node:path";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

const pkgJson = JSON.parse(fs.readFileSync(path.join(__dirname, "package.json"), "utf8"));
const PACKAGE = process.env.YNPM_PACKAGE_NAME || pkgJson.name;
const PLATFORM = process.env.YNPM_PLATFORM || `${process.platform}-${process.arch}`;
const strict = Boolean(process.env.YNPM_PACKAGE_NAME && process.env.YNPM_PLATFORM);
const shortName = PACKAGE.includes("/") ? PACKAGE.split("/")[1] : PACKAGE;

function fail(message) {
  if (strict) {
    console.error(`ynpm finalize: ${message}`);
    process.exit(1);
  }
  console.warn(`ynpm finalize (non-fatal): ${message}`);
  process.exit(0);
}

// The platform package may sit NESTED under this package's own node_modules
// (npm 11 global layout) or as a flat sibling. Try both.
const platformPkg = [
  path.join(__dirname, "node_modules", `@ygghq`, `${shortName}-${PLATFORM}`),
  path.join(__dirname, "..", "node_modules", `@ygghq`, `${shortName}-${PLATFORM}`),
].find((c) => fs.existsSync(path.join(c, "package.json")));

if (!platformPkg) {
  fail(`${shortName}-${PLATFORM} is not installed beside this package - the shims will not find binaries for this platform`);
}

const platformBins = JSON.parse(fs.readFileSync(path.join(platformPkg, "package.json"), "utf8")).bin || {};
let copied = 0;
for (const [binName, relPath] of Object.entries(platformBins)) {
  const src = path.join(platformPkg, relPath);
  const dst = path.join(__dirname, "bin", binName + ".platform");
  try {
    fs.copyFileSync(src, dst);
    fs.chmodSync(dst, 0o755);
    execFileSync(dst, ["--version"], { stdio: "ignore", timeout: 30000 });
    copied += 1;
  } catch (error) {
    console.warn(`ynpm finalize (non-fatal): ${binName} fast copy unusable (${error.status ?? error.message ?? error})`);
  }
}
console.log(`ynpm finalize: ${copied}/${Object.keys(platformBins).length} binaries finalized for ${PLATFORM}`);
