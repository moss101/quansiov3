# APP-003 — the theme chosen in Settings is applied

The personal theme (Settings → Just for you → Theme) was stored but the desktop only followed the OS.
The shell applies the effective theme from /v1/me on opening, Settings re-applies it after a save
(`src/appearance.ts`, `data-theme` on the root; `index.css` keys dark tokens on a chosen dark theme or the
OS for system), and the `appearance.set` channel (system/light/dark only) sets Electron's nativeTheme.

Tests (`desktop-suite.log`, 155/155 against the dev stack, serial): `appearance.test.ts`, the channel's
validation in `shell-modules.test.ts`, and a session journey: dark on a light system (page and
nativeTheme), the choice kept on the next launch, light on a dark system, back to the system. Mutants —
the shell not applying on launch, dark tokens not keyed on the choice, main not setting nativeTheme —
each fail the journey; the control passes.
