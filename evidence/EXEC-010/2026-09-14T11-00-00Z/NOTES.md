# Evidence notes

Narrative that the canonical `summary.json` schema does not carry (DOSSIER.md 19 governs the summary;
a bundle may carry other evidence files).

## acceptance

**Unknown foreground app fails closed for privileged action.** Enforced in
`crates/machine/src/computer_use/`: the foreground application's identity is what the decision is about, so
an application this runtime does not recognise is refused *before* the tier is considered — a run granted
`system_key` included. There is deliberately no tier for which guessing about the foreground app is safe,
which is what "fails closed" has to mean. Identity matches on the bundle identifier and not the display
name, because an application can call itself anything; the test drives an impostor named "TextEdit" with a
different bundle id and asserts it is refused. A missing foreground application is its own refusal rather
than being treated as unknown, and `KnownApps::none()` recognises nothing, which is what an uninformed run
must get.

**User takeover cannot race with agent input.** `AutomationFence` serializes them with a generation plus a
drain rather than a lock held across an action: `begin` refuses unless the agent holds the machine, counts
the action in flight and returns the generation it acts under; `request_takeover` fences new actions
immediately; `complete_takeover` refuses while anything is in flight, so a person cannot take control
while an action is landing; and `finish` refuses to close a generation the fence has moved past, so an
action cannot report acting under a fence that was superseded. Holding a mutex across the action itself
would be wrong — actions are long and asynchronous, and a lock held for their duration would make a
takeover wait on the very thing it exists to interrupt. The property is driven with real concurrency
(eight spawned actions against a takeover that retries until the drain completes) and asserts that no
action is in flight when control changes and that every action is accounted for as either run or fenced.

## the boundary is real, and it is this Mac's accessibility API

`native/macos` gained the AX/input surface the package README already said belonged there. The live probe
(`boundary-probe.log`) establishes the boundary rather than assuming it:

```
frontmost: ZCode bundle=dev.zcode.app pid=713
running apps: 85
AX role of frontmost: err=0 value=Optional(AXApplication)
AX windows: err=0 count=2
system-wide focused app: err=-25204 (kAXErrorCannotComplete=-25204, kAXErrorAPIDisabled=-25211)
```

`AXIsProcessTrusted` is **true** on this host and Screen Recording is granted (23 on-screen window titles
are readable), so the privileged calls work. The system-wide element's focused-application attribute
answers `kAXErrorCannotComplete` on a host with no attached GUI session — which is why identity comes from
`NSWorkspace.frontmostApplication` instead: it answers in exactly that situation, and it is the identity
the decision needs. That choice is documented at the call site.

The Swift suite is nine tests, five of them new: identity resolves without Accessibility, a tree walk is
bounded in nodes and depth, unusable bounds are typed refusals, the clipboard round-trips *and is put
back*, and screen-capture availability agrees with whether window titles are readable. The tests
deliberately do **not** post input events or write the clipboard without restoring it: the machine running
them is somebody's, and a suite that moves the pointer to prove a function exists has damaged the thing it
was measuring. `computer.click`/`computer.type`/`computer.setClipboardText` are therefore exercised for
their event construction and their typing, not by driving a real pointer.

## the Windows substrate is not this task's, and is not done

The build item names app identity "via AX (macOS) and UIA (Windows)". The macOS half is implemented and
verified. The Windows half is **not implemented and not verified here**, and this is recorded plainly
rather than glossed: `native/windows` exists but no Windows host is available, and the task graph assigns
the Windows native broker to **EXEC-004 — "Implement Windows local capsule and native broker"**, which
owns `native/windows/`. So the position is that EXEC-010's macOS substrate and its whole policy surface
(which is substrate-neutral and where both acceptance statements live) are complete, and the Windows UIA
binding arrives with EXEC-004. If a reviewer reads the build item as EXEC-010's own, then this task is
incomplete on that one line and should be reopened when a Windows runner exists — the registry and
`HANDOFF.md` both say so.

## recorded_decisions

- **The privileged calls stay out of Rust.** `crates/machine` is `#![forbid(unsafe_code)]`, and the AX and
  `CGEvent` calls are C APIs; a Swift bridge in `native/macos` — the canonical owner the README already
  names for AX inspection — keeps the unsafe surface out of the Rust authority and leaves the operator a
  single binary to grant the TCC permission to. The bridge decides nothing: it answers questions and posts
  events.
- **A tree read is bounded**, because an application's accessibility tree is arbitrarily large and a bridge
  that copies all of it can be made to allocate without limit. The walk is breadth-first so the bound drops
  the deepest levels first, which are the least likely to hold the control a caller wants, and `truncated`
  says when a bound rather than the tree ended the walk.
- **The tiers are a total order, not a set.** `Read < Click < Type < Clipboard < SystemKey`, so
  `ComputerGrant::covers` is a comparison rather than a list to keep in step, and every tier names the tool
  that acts at it so a refusal can say which call it refused.
