// Quansio V8.1 lint configuration (DOSSIER.md §18). TypeScript strict mode is the
// baseline; renderer code must never reach runtime/effect/database authority, which
// scripts/ci/inventory.py --scan enforces independently.
import tseslint from "typescript-eslint";

export default tseslint.config(
  { ignores: ["**/dist/**", "**/node_modules/**", "**/.pnpm-store/**"] },
  ...tseslint.configs.strictTypeChecked,
  {
    languageOptions: {
      parserOptions: { projectService: true, tsconfigRootDir: import.meta.dirname },
    },
  },
  {
    files: ["**/*.config.js", "**/eslint.config.js"],
    ...tseslint.configs.disableTypeChecked,
  },
);
