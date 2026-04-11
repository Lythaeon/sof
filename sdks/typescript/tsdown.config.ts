import { defineConfig } from "tsdown";

export default defineConfig({
  clean: true,
  dts: true,
  entry: {
    index: "src/index.ts",
    runtime: "src/runtime.ts",
    "runtime/config": "src/runtime/runtime-config.ts",
    "runtime/policy": "src/runtime/runtime-policy.ts",
    "runtime/derived-state": "src/runtime/derived-state.ts",
    "runtime/delivery-profile": "src/runtime/runtime-delivery-profile.ts",
    "runtime/extension": "src/runtime/runtime-extension.ts",
  },
  format: ["esm"],
  minify: true,
  outDir: "dist",
  platform: "neutral",
  sourcemap: true,
  target: "es2022",
  unbundle: true,
});
