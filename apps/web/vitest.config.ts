import { defineConfig, type ViteUserConfig } from "vitest/config";

// Emitted ESM imports use `.js` specifiers; alias them back to TypeScript sources so
// tests exercise the real modules rather than a build artifact. `extensionAlias` is a
// Vite resolve option supported at runtime here but missing from the published type.
const resolve = {
  extensionAlias: { ".js": [".ts", ".js"] },
} as unknown as NonNullable<ViteUserConfig["resolve"]>;

export default defineConfig({
  resolve,
  test: { include: ["tests/**/*.test.ts"] },
});
