import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    name: "qa-e2e",
    include: ["tests/e2e/**/*.spec.ts"],
    environment: "node",
  },
});
