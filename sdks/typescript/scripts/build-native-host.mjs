import { chmodSync, copyFileSync, existsSync, mkdirSync } from "node:fs";
import { join } from "node:path";
import { spawnSync } from "node:child_process";

const scriptDirectory = import.meta.dirname;
const packageDirectory = join(scriptDirectory, "..");
const workspaceDirectory = join(packageDirectory, "..", "..");
const binaryName = process.platform === "win32" ? "sof_ts_runtime_host.exe" : "sof_ts_runtime_host";
const targetTriple = `${process.platform}-${process.arch}`;
const sourcePath = join(workspaceDirectory, "target", "release", binaryName);
const outputDirectory = join(packageDirectory, "dist", "native", targetTriple);
const outputPath = join(outputDirectory, binaryName);
const features = ["provider-grpc", "gossip-bootstrap"];

if (process.platform === "linux") {
  features.push("kernel-bypass");
}

const cargo = spawnSync(
  "cargo",
  [
    "build",
    "--release",
    "-p",
    "sof",
    "--features",
    features.join(","),
    "--bin",
    "sof_ts_runtime_host",
  ],
  {
    cwd: workspaceDirectory,
    stdio: "inherit",
  },
);

if (cargo.error !== undefined) {
  throw new Error(`failed to run cargo: ${cargo.error.message}`);
}

if (cargo.status !== 0) {
  process.exitCode = cargo.status ?? 1;
  throw new Error(`cargo build failed with status ${String(cargo.status)}`);
}

if (!existsSync(sourcePath)) {
  throw new Error(`native runtime host binary was not produced at ${sourcePath}`);
}

mkdirSync(outputDirectory, { recursive: true });
copyFileSync(sourcePath, outputPath);
chmodSync(outputPath, 0o755);
