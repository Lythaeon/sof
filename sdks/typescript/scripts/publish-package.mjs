import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { join } from "node:path";

function fail(message) {
  throw new Error(message);
}

const packageJson = JSON.parse(readFileSync(join(process.cwd(), "package.json"), "utf8"));
const packageName = packageJson.name;
const packageVersion = packageJson.version;

if (typeof packageName !== "string" || packageName.length === 0) {
  fail("package.json must define a package name");
}

if (typeof packageVersion !== "string" || packageVersion.length === 0) {
  fail("package.json must define a package version");
}

const packageSpec = `${packageName}@${packageVersion}`;
const view = spawnSync("npm", ["view", packageSpec, "version"], {
  stdio: "ignore",
});

if (view.error !== undefined) {
  fail(`failed to check npm registry for ${packageSpec}: ${view.error.message}`);
}

if (view.status === 0) {
  process.stdout.write(`${packageSpec} is already published; skipping npm publish.\n`);
} else {
  const publish = spawnSync("pnpm", ["publish", "--access", "public", "--no-git-checks"], {
    stdio: "inherit",
  });

  if (publish.error !== undefined) {
    fail(`failed to run pnpm publish for ${packageSpec}: ${publish.error.message}`);
  }

  if (publish.status !== 0) {
    process.exitCode = publish.status ?? 1;
    fail(`pnpm publish failed for ${packageSpec} with status ${String(publish.status)}`);
  }
}
