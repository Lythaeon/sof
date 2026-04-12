# Releasing `@lythaeon-sof/sdk`

The TypeScript SDK publishes as one main package plus six platform-native runtime packages.

## Release Units

- `@lythaeon-sof/sdk`
- `@lythaeon-sof/sdk-native-linux-x64`
- `@lythaeon-sof/sdk-native-linux-arm64`
- `@lythaeon-sof/sdk-native-darwin-x64`
- `@lythaeon-sof/sdk-native-darwin-arm64`
- `@lythaeon-sof/sdk-native-win32-x64`
- `@lythaeon-sof/sdk-native-win32-arm64`

All seven packages must share the same version.

## Tag Convention

Use `ts-sdk-vX.Y.Z` tags for npm releases.

Examples:

```sh
git tag ts-sdk-v0.1.0
git push origin ts-sdk-v0.1.0
```

The release workflow rejects tags that do not match `sdks/typescript/package.json`.

## Publish Order

The release workflow publishes in this order:

1. Verify the tagged commit is on `main`.
2. Run `pnpm --dir sdks/typescript run check`.
3. Publish each native runtime package on its matching platform runner.
4. Wait for all native runtime packages to appear on npm.
5. Publish `@lythaeon-sof/sdk`.

This order matters because `@lythaeon-sof/sdk` depends on the native packages through optional dependencies.

## Local Verification

Run this before tagging:

```sh
pnpm install --frozen-lockfile
pnpm --dir sdks/typescript run check
```

For a workflow-only dry run without publishing:

1. Open the `Release TypeScript SDK` workflow in GitHub Actions.
2. Run it with `publish=false`.

That path builds and packs the packages without uploading them to npm.
