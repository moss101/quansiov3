// Bundle the web app (APP-012): one ES module and one stylesheet under dist/web/assets,
// served by quansio-server under /app/ (QUANSIO_WEB_ROOT) or by a CDN. Browser platform,
// no Node access, no external scripts — the server's CSP allows this origin only.
import { build } from "esbuild";
import { copyFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const out = join(root, "dist", "web");
mkdirSync(join(out, "assets"), { recursive: true });

await build({
  entryPoints: [join(root, "src/app/main.tsx")],
  outfile: join(out, "assets", "app.js"),
  bundle: true,
  platform: "browser",
  format: "esm",
  target: "es2022",
  jsx: "automatic",
  minify: true,
  sourcemap: true,
  define: { "process.env.NODE_ENV": '"production"' },
});
copyFileSync(join(root, "src/app/index.html"), join(out, "index.html"));
copyFileSync(join(root, "src/app/app.css"), join(out, "assets", "app.css"));
console.log(`web bundle written to ${out}`);
