// Quansio V8.1 lint configuration (DOSSIER.md §18). TypeScript strict mode is the
// baseline; renderer code must never reach runtime/effect/database authority, which
// scripts/ci/inventory.py --scan enforces independently.
import tseslint from "typescript-eslint";

export default tseslint.config(
  // A worktree or clone below the root is another checkout's tree (see
  // scripts/ci/supply_chain/tree.py for the same rule in the supply-chain scanners).
  { ignores: ["**/dist/**", "**/node_modules/**", "**/.pnpm-store/**", "**/.claude/worktrees/**"] },
  ...tseslint.configs.strictTypeChecked,
  {
    languageOptions: {
      parserOptions: { projectService: true, tsconfigRootDir: import.meta.dirname },
    },
  },
  {
    files: ["**/*.config.js", "**/eslint.config.js", "**/scripts/*.mjs"],
    ...tseslint.configs.disableTypeChecked,
  },
);
