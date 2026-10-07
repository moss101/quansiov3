// Bundle the desktop for Electron (APP-003): main and preload as CommonJS for the Node side,
// the renderer as an ES module served from the `app:` scheme. `electron` and `ws` stay
// external in the Node bundles; the renderer bundle has no Node access at all.
import { build } from "esbuild";
import { copyFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const out = join(root, "dist", "app");
mkdirSync(join(out, "renderer"), { recursive: true });

await build({
  entryPoints: [join(root, "src/main/app.ts")],
  outfile: join(out, "main.cjs"),
  bundle: true,
  platform: "node",
  format: "cjs",
  target: "node22",
  external: ["electron", "ws"],
  sourcemap: true,
  // `app.ts` locates the renderer from `import.meta.url`; a CommonJS bundle has none, so
  // the bundle derives it from `__filename` and would otherwise crash on startup.
  banner: { js: "const __quansio_import_meta_url = require('node:url').pathToFileURL(__filename).href;" },
  define: { "import.meta.url": "__quansio_import_meta_url" },
});
await build({
  entryPoints: [join(root, "src/preload/index.ts")],
  outfile: join(out, "preload.cjs"),
  bundle: true,
  platform: "node",
  format: "cjs",
  target: "node22",
  external: ["electron"],
  sourcemap: true,
});
await build({
  entryPoints: [join(root, "src/renderer/index.tsx")],
  outfile: join(out, "renderer", "index.js"),
  bundle: true,
  platform: "browser",
  format: "esm",
  target: "es2022",
  jsx: "automatic",
  sourcemap: true,
});
copyFileSync(join(root, "src/renderer/index.html"), join(out, "renderer", "index.html"));
copyFileSync(join(root, "src/renderer/index.css"), join(out, "renderer", "index.css"));
// The shell's policy values (ADP-004, D-018): the main process reads this file beside its bundle.
copyFileSync(join(root, "..", "..", "config", "desktop-shell.yaml"), join(out, "desktop-shell.yaml"));
// The local-file limits (ADP-005, D-018): read the same way.
copyFileSync(join(root, "..", "..", "config", "local-files.yaml"), join(out, "local-files.yaml"));
// The local-secrets limits (ADP-009, D-018): read the same way.
copyFileSync(join(root, "..", "..", "config", "local-secrets.yaml"), join(out, "local-secrets.yaml"));
console.log(`desktop bundle written to ${out}`);
