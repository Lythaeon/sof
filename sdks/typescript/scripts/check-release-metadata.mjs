import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";

const scriptDirectory = import.meta.dirname;
const sdkDirectory = join(scriptDirectory, "..");
const nativeDirectory = join(sdkDirectory, "native");

function readJson(path) {
  return JSON.parse(readFileSync(path, "utf8"));
}

function fail(message) {
  throw new Error(message);
}

const sdkPackage = readJson(join(sdkDirectory, "package.json"));
const nativePackageDirectories = readdirSync(nativeDirectory, { withFileTypes: true })
  .filter((entry) => entry.isDirectory())
  .map((entry) => entry.name)
  .toSorted();

if (nativePackageDirectories.length === 0) {
  fail("no native package directories were found under sdks/typescript/native");
}

const optionalDependencyNames = Object.keys(sdkPackage.optionalDependencies ?? {}).toSorted();
const nativePackages = nativePackageDirectories.map((directoryName) => {
  const packageJsonPath = join(nativeDirectory, directoryName, "package.json");
  const packageJson = readJson(packageJsonPath);

  if (packageJson.version !== sdkPackage.version) {
    fail(
      `native package ${packageJson.name} has version ${packageJson.version}, expected ${sdkPackage.version}`,
    );
  }

  if (packageJson.publishConfig?.access !== "public") {
    fail(`native package ${packageJson.name} must set publishConfig.access to public`);
  }

  if (packageJson.publishConfig?.provenance !== true) {
    fail(`native package ${packageJson.name} must set publishConfig.provenance to true`);
  }

  if (!Array.isArray(packageJson.files) || !packageJson.files.includes("vendor")) {
    fail(`native package ${packageJson.name} must publish the vendor directory`);
  }

  if (packageJson.scripts?.prepack !== "node ../../scripts/build-native-host.mjs") {
    fail(`native package ${packageJson.name} must build through scripts/build-native-host.mjs`);
  }

  if (packageJson.repository?.type !== sdkPackage.repository?.type) {
    fail(`native package ${packageJson.name} must mirror the sdk repository metadata`);
  }

  if (packageJson.repository?.url !== sdkPackage.repository?.url) {
    fail(`native package ${packageJson.name} must mirror the sdk repository URL`);
  }

  if (packageJson.repository?.directory !== `sdks/typescript/native/${directoryName}`) {
    fail(
      `native package ${packageJson.name} must point repository.directory to sdks/typescript/native/${directoryName}`,
    );
  }

  if (packageJson.bugs?.url !== sdkPackage.bugs?.url) {
    fail(`native package ${packageJson.name} must mirror the sdk bugs URL`);
  }

  if (packageJson.homepage !== sdkPackage.homepage) {
    fail(`native package ${packageJson.name} must mirror the sdk homepage`);
  }

  return packageJson;
});

const nativePackageNames = nativePackages.map((packageJson) => packageJson.name).toSorted();

if (JSON.stringify(optionalDependencyNames) !== JSON.stringify(nativePackageNames)) {
  fail(
    `sdk optionalDependencies ${JSON.stringify(optionalDependencyNames)} do not match native packages ${JSON.stringify(nativePackageNames)}`,
  );
}

for (const packageJson of nativePackages) {
  if (sdkPackage.optionalDependencies?.[packageJson.name] !== "workspace:*") {
    fail(`sdk optional dependency ${packageJson.name} must stay on workspace:* in source control`);
  }
}

process.stdout.write(
  `${JSON.stringify(
    {
      sdkVersion: sdkPackage.version,
      nativePackages: nativePackages.map((packageJson) => packageJson.name),
    },
    undefined,
    2,
  )}\n`,
);
