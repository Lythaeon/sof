import { spawnSync } from "node:child_process";
import { resolve } from "node:path";

function fail(message) {
  throw new Error(message);
}

function readFlag(name) {
  const flag = `--${name}`;
  const index = process.argv.indexOf(flag);
  if (index === -1) {
    return;
  }

  const value = process.argv[index + 1];
  if (value === undefined || value.startsWith("--")) {
    fail(`missing value for ${flag}`);
  }

  return value;
}

const packageDirectoryFlag = readFlag("package-dir");
if (packageDirectoryFlag === undefined) {
  fail("expected --package-dir");
}

const platform = readFlag("platform");
const arch = readFlag("arch");
const target = readFlag("target");
const cargoSubcommand = readFlag("cargo-subcommand");
const packageDirectory = resolve(packageDirectoryFlag);

const child = spawnSync("pnpm", ["publish", "--dry-run", "--access", "public", "--no-git-checks"], {
  cwd: packageDirectory,
  env: {
    ...process.env,
    ...(platform === undefined ? {} : { SOF_TS_NATIVE_PLATFORM: platform }),
    ...(arch === undefined ? {} : { SOF_TS_NATIVE_ARCH: arch }),
    ...(target === undefined ? {} : { SOF_TS_NATIVE_TARGET: target }),
    ...(cargoSubcommand === undefined ? {} : { SOF_TS_NATIVE_CARGO_SUBCOMMAND: cargoSubcommand }),
  },
  stdio: "inherit",
});

if (child.error !== undefined) {
  fail(`failed to run pnpm: ${child.error.message}`);
}

if (child.status !== 0) {
  process.exitCode = child.status ?? 1;
  fail(`pnpm publish --dry-run failed with status ${String(child.status)}`);
}
