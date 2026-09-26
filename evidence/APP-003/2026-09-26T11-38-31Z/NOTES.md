# APP-003 regression fix — the renderer did not load in the real Electron binary

Found by QA-008 on 2026-09-26, on the first launch of the real binary: `app://desktop/index.html`
failed with `ERR_UNEXPECTED` because the `app:` protocol handler used Node's global `fetch`, which
cannot read `file:` URLs ("not implemented... yet"). No test had ever launched the binary: pnpm
never ran electron's install script (`allowBuilds`), so `node_modules/electron/dist` did not exist
and every desktop test drove the bridge and renderer modules in-process.

Fix: `net.fetch` (Electron's fetch, which reads `file:` URLs). Regression test:
`apps/desktop/tests/launch.e2e.test.ts` launches the binary through Playwright's Electron driver and
waits for the sign-in form — it fails on the previous handler (`TypeError: fetch failed`) and passes
on the fix. The full desktop suite passes (29 files, 121 tests).
