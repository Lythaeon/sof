import { spawnSync } from "node:child_process";
import { chmodSync, copyFileSync, existsSync, mkdirSync, readFileSync } from "node:fs";
import { basename, join } from "node:path";

const scriptDirectory = import.meta.dirname;
const sdkPackageDirectory = join(scriptDirectory, "..");
const workspaceDirectory = join(sdkPackageDirectory, "..", "..");

function runtimeHostBinaryName() {
  return process.platform === "win32" ? "sof_ts_runtime_host.exe" : "sof_ts_runtime_host";
}

function nativePackageDirectory() {
  const cwdPackagePath = join(process.cwd(), "package.json");
  if (existsSync(cwdPackagePath)) {
    const cwdPackage = JSON.parse(readFileSync(cwdPackagePath, "utf8"));
    if (
      typeof cwdPackage.name === "string" &&
      cwdPackage.name.startsWith("@lythaeon-sof/sdk-native-")
    ) {
      return process.cwd();
    }
  }

  return join(sdkPackageDirectory, "native", `${process.platform}-${process.arch}`);
}

function packageMetadata(packageDirectory) {
  const packageJsonPath = join(packageDirectory, "package.json");
  if (!existsSync(packageJsonPath)) {
    throw new Error(
      `native package metadata was not found at ${packageJsonPath}; expected a package for ${process.platform}-${process.arch}`,
    );
  }

  return JSON.parse(readFileSync(packageJsonPath, "utf8"));
}

const packageDirectory = nativePackageDirectory();
const packageJson = packageMetadata(packageDirectory);
const outputDirectory = join(packageDirectory, "vendor");
const outputPath = join(outputDirectory, runtimeHostBinaryName());
const sourcePath = join(workspaceDirectory, "target", "release", runtimeHostBinaryName());
const features = ["provider-websocket", "provider-grpc", "gossip-bootstrap"];

if (process.platform === "linux") {
  features.push("kernel-bypass");
}

const packagePlatform = basename(packageDirectory);
if (packagePlatform !== `${process.platform}-${process.arch}`) {
  throw new Error(
    `native package ${packageJson.name ?? packagePlatform} does not match the current platform ${process.platform}-${process.arch}`,
  );
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
if (process.platform !== "win32") {
  chmodSync(outputPath, 0o755);
}
