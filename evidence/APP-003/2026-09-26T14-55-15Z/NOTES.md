# APP-003 — a chat-first desktop shell (raised during QA-008)

The owner compared the desktop with the macOS assistant apps running beside it: "frontend
design does not match with peer applications". The 14:17 design (`before-d-chat.png`) read as an
admin console: a 15-link sidebar, a top bar, three bordered columns, raw thread ids, a purple accent.
The owner chose the full chat-first layout.

- Window: no title bar on macOS (`titleBarStyle: hiddenInset`, traffic lights inset over the
  sidebar); the sidebar top row and pane header are the drag regions.
- Sidebar: conversations (avatar, title, message count), search filter, + for a new one; a
  Chats/Workspace switch shows every surface (the `Surfaces` navigation, still one click away);
  the account menu holds Settings and Sign out.
- Pane: the open conversation fills it — bubbles (ink for the person, grey for teammates),
  questions as in-thread action cards, a pill composer (+ attach, Return sends, Shift+Return
  breaks, stop, send); the inspector and the event stream open as panels on demand.
- Tokens: neutral ink-on-paper light/dark palette, 15px system type, rounder controls; every
  other surface picks them up (`d-approvals.png`). Keyboard focus stays visible (the composer
  pill rings when its field has keyboard focus).

Screenshots are the real Electron app against the real server (1280×860): `d-chat.png`,
`d-chat-dark.png`, `d-approvals.png`. The peer app was compared on screen; its screenshot is not
kept (it shows a personal account). Tests: the full desktop suite with the dev stack
(`vitest-desktop.log`, 130/130; run serially — under a load average above 20 seven bridge E2Es
hit their 30 s timeouts in parallel). The new session journey (a message's conversation appears
in the sidebar, + starts another) fails when the conversation stops reporting the thread it
created (mutation checked). Not changed here: a finished run's step bubble and stop button can
linger until the run's terminal event is read.
