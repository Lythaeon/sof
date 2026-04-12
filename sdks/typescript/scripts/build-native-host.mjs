import { spawnSync } from "node:child_process";
import { chmodSync, copyFileSync, existsSync, mkdirSync, readFileSync } from "node:fs";
import { basename, delimiter, join } from "node:path";

const scriptDirectory = import.meta.dirname;
const sdkPackageDirectory = join(scriptDirectory, "..");
const workspaceDirectory = join(sdkPackageDirectory, "..", "..");
const cargoTargetRoot = process.env.CARGO_TARGET_DIR ?? join(workspaceDirectory, "target");
const overriddenPlatform = process.env.SOF_TS_NATIVE_PLATFORM;
const overriddenArch = process.env.SOF_TS_NATIVE_ARCH;
const overriddenTarget = process.env.SOF_TS_NATIVE_TARGET;
const cargoSubcommand = process.env.SOF_TS_NATIVE_CARGO_SUBCOMMAND;
const crossBuildCargoSubcommand = "zigbuild";

function packagePlatformFromDirectory(packageDirectory) {
  const name = basename(packageDirectory);
  const separator = name.indexOf("-");
  if (separator === -1) {
    return;
  }

  return {
    arch: name.slice(separator + 1),
    platform: name.slice(0, separator),
  };
}

function buildPlatform(packageDirectory) {
  return (
    overriddenPlatform ??
    packagePlatformFromDirectory(packageDirectory)?.platform ??
    process.platform
  );
}

function buildArch(packageDirectory) {
  return overriddenArch ?? packagePlatformFromDirectory(packageDirectory)?.arch ?? process.arch;
}

function runtimeHostBinaryName(platform) {
  return platform === "win32" ? "sof_ts_runtime_host.exe" : "sof_ts_runtime_host";
}

function defaultCargoTarget(platform, arch) {
  if (platform === process.platform && arch === process.arch) {
    return;
  }

  if (platform === "darwin" && arch === "x64") {
    return "x86_64-apple-darwin";
  }
  if (platform === "darwin" && arch === "arm64") {
    return "aarch64-apple-darwin";
  }
  if (platform === "win32" && arch === "x64") {
    return "x86_64-pc-windows-gnu";
  }
  if (platform === "win32" && arch === "arm64") {
    return "aarch64-pc-windows-gnullvm";
  }
  if (platform === "linux" && arch === "arm64") {
    return "aarch64-unknown-linux-gnu";
  }
}

function defaultCargoSubcommand(target) {
  if (target !== undefined) {
    return crossBuildCargoSubcommand;
  }
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
      `native package metadata was not found at ${packageJsonPath}; expected a package for ${buildPlatform()}-${buildArch()}`,
    );
  }

  return JSON.parse(readFileSync(packageJsonPath, "utf8"));
}

const packageDirectory = nativePackageDirectory();
const packageJson = packageMetadata(packageDirectory);
const platform = buildPlatform(packageDirectory);
const arch = buildArch(packageDirectory);
const target = overriddenTarget ?? defaultCargoTarget(platform, arch);
const subcommand = cargoSubcommand ?? defaultCargoSubcommand(target);
const outputDirectory = join(packageDirectory, "vendor");
const binaryName = runtimeHostBinaryName(platform);
const targetDirectory = target === undefined ? "release" : join(target, "release");
const outputPath = join(outputDirectory, binaryName);
const sourcePath = join(cargoTargetRoot, targetDirectory, binaryName);
const features = ["provider-websocket", "provider-grpc", "gossip-bootstrap"];

if (platform === "linux") {
  features.push("kernel-bypass");
}

const packagePlatform = basename(packageDirectory);
const currentPlatformKey = `${platform}-${arch}`;
if (packagePlatform !== currentPlatformKey) {
  throw new Error(
    `native package ${packageJson.name ?? packagePlatform} does not match the selected platform ${currentPlatformKey}`,
  );
}

const cargoArgs = [];
if (subcommand !== undefined && subcommand.length > 0) {
  cargoArgs.push(subcommand);
} else {
  cargoArgs.push("build");
}
if (target !== undefined) {
  cargoArgs.push("--target", target);
}
cargoArgs.push(
  "--release",
  "-p",
  "sof",
  "--features",
  features.join(","),
  "--bin",
  "sof_ts_runtime_host",
);

const cargoEnv = { ...process.env };
cargoEnv.PATH = [join(sdkPackageDirectory, "node_modules", ".bin"), cargoEnv.PATH]
  .filter((value) => value !== undefined && value.length > 0)
  .join(delimiter);
if (platform === "darwin") {
  cargoEnv.CARGO_PROFILE_RELEASE_LTO = "off";
}

const cargo = spawnSync("cargo", cargoArgs, {
  cwd: workspaceDirectory,
  env: cargoEnv,
  stdio: "inherit",
});

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
if (platform !== "win32") {
  chmodSync(outputPath, 0o755);
}
