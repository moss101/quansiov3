import { defineConfig, type ViteUserConfig } from "vitest/config";

// Emitted ESM imports use `.js` specifiers; alias them back to TypeScript sources so
// tests exercise the real modules rather than a build artifact. `extensionAlias` is a
// Vite resolve option supported at runtime here but missing from the published type.
const resolve = {
  extensionAlias: { ".js": [".ts", ".js"] },
} as unknown as NonNullable<ViteUserConfig["resolve"]>;

// Under the baseline pipeline each test's outcome is also written as JSON, so its test audit
// can hold every skip to the BLOCKED_EXTERNAL convention (QA-001).
const ciLogs = process.env["QUANSIO_CI_LOG_DIR"];

export default defineConfig({
  resolve,
  test: {
    include: ["tests/**/*.test.ts", "tests/**/*.test.tsx"],
    ...(ciLogs
      ? { reporters: ["default", "json"], outputFile: { json: `${ciLogs}/vitest-web.json` } }
      : {}),
  },
});
