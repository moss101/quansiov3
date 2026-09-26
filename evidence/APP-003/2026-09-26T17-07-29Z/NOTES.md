# APP-003 — desktop design refresh

The owner found the chat-first shell (`before-d-chat.png`) still did not look like the macOS
assistant apps running beside it. That app's shipped renderer was measured — design tokens, the
geometry and states of its sidebar rows, header, bubbles, cards, composer, menus and dialogs, its
type scale and motion — and the desktop was re-implemented from those measurements in Quansio's
own code. Nothing was copied: no source, fonts, mascots or icons; faces are letters on tones and
the icon set is drawn for Quansio.

What changed (`apps/desktop/src/renderer/`):

- Tokens (`index.css`): #fcfcfc canvas, #f7f7f7 sidebar, #070707 primary, alpha hairlines, full
  dark theme, 420-weight system type on a 14/13/12/11 scale, blue only for links, focus and unread.
- Sidebar (`Sidebar.tsx`, `Face.tsx`): 54px rows with teammate faces (group frames, +N), latest
  message, "Waiting for you" (server attention projection) and unread (this session) dots;
  Workspace lists every surface with icons; account menu from the person's initials.
- Header: floating name island (opens the details pane); transcript fades beneath it.
- Transcript (`Conversation.tsx`, `Prose.tsx`, `conversation/presentation.ts`): grouped bubbles,
  time dividers (15 min or a new day), sender names only in multi-teammate threads, parsed text
  (never HTML; only http(s) links), working row, question action cards, jump to latest.
- Composer: 44px pill growing into a box; "Message <teammate>"; addressee chips for a new thread.
- Command palette (`Palette.tsx`, Cmd/Ctrl+K).

Fixed on the way: a conversation's run stays RUNNING between turns (seen from the real server's
run timeline), so the working row and Cancel lingered after every reply; they now follow the turn
in flight (`turnInFlight`), and PostMessage's run status / the timeline settle a run whose
terminal event arrived first.

Tests: full desktop suite against the dev stack, run serially (`vitest-desktop.log`, 151/151, no
skips). New: `presentation.test.ts` (rows, faces, dividers, turn in flight), `renderer-parts.test.ts`
(text parsing incl. non-http schemes stay text, palette matching, thread decoding), a reducer
regression in `conversation.test.ts`, and session E2E journeys (sidebar preview, no lingering
work after a reply, palette navigation). Mutation checks against the real app: blanking previews
and restoring the old "open run = working" rule each fail the journey; controls pass.

Screenshots: real Electron against the real server at 1040×760 — `d-chat.png`, `d-chat-dark.png`,
`d-new.png` (new conversation), `d-palette.png`, `d-page.png` (a Workspace page).
