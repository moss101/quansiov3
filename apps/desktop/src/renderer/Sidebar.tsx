/**
 * The sidebar (APP-003): the person's conversations — who each is with, the latest message and
 * whether a teammate is waiting on them — or, switched to Workspace, every surface of the
 * workspace. Pure presentation: rows arrive computed (`conversation/presentation.ts`) and every
 * click is a navigation the shell performs.
 */

import { useEffect, useRef, useState, type ReactElement } from "react";

import type { SidebarRow } from "../conversation/presentation.js";
import { t } from "../i18n/strings.js";
import { DESKTOP_SURFACES } from "../surfaces.js";
import { Faces, Initials } from "./Face.js";
import { ChatIcon, GearIcon, GridIcon, LeaveIcon, PlusIcon, SearchIcon } from "./icons.js";
import { OPENS, PAGE_ENTRY, SURFACE_ICONS, SURFACE_LABELS, type Page } from "./navigation.js";

export type SidebarView = "chats" | "workspace";

interface SidebarProps {
  readonly view: SidebarView;
  readonly onView: (view: SidebarView) => void;
  readonly rows: readonly SidebarRow[];
  readonly openThreadId: string | null;
  readonly page: Page;
  readonly onOpenThread: (threadId: string | null) => void;
  readonly onOpenPage: (page: Page) => void;
  readonly onSearch: () => void;
  /** A conversation was dragged out of the window: open it in its own window, at the drop point when known. */
  readonly onPopout: (threadId: string, point: { readonly x: number; readonly y: number } | null) => void;
  readonly initials: string;
  readonly onSignOut: () => void;
}

export function Sidebar(props: SidebarProps): ReactElement {
  const { view, onView, rows, openThreadId, page, onOpenThread, onOpenPage, onSearch, onPopout, initials, onSignOut } = props;
  const [accountOpen, setAccountOpen] = useState(false);
  const anchor = useRef<HTMLDivElement | null>(null);

  // The account menu closes on Escape and on any press outside it.
  useEffect(() => {
    if (!accountOpen) {
      return;
    }
    const onKey = (event: KeyboardEvent): void => {
      if (event.key === "Escape") {
        setAccountOpen(false);
      }
    };
    const onPress = (event: MouseEvent): void => {
      if (anchor.current && !anchor.current.contains(event.target as Node)) {
        setAccountOpen(false);
      }
    };
    document.addEventListener("keydown", onKey);
    document.addEventListener("mousedown", onPress);
    return () => {
      document.removeEventListener("keydown", onKey);
      document.removeEventListener("mousedown", onPress);
    };
  }, [accountOpen]);

  return (
    <aside className="sidebar">
      <h1 className="sr-only">{t("appName")}</h1>
      <div className="sidebar-top">
        <button type="button" className="icon-button elevated" aria-label={t("searchConversations")} data-tip={t("searchConversations")} onClick={onSearch}>
          <SearchIcon />
        </button>
        <button type="button" className="icon-button elevated" aria-label={t("newThread")} data-tip={t("newThread")} onClick={() => { onOpenThread(null); }}>
          <PlusIcon />
        </button>
      </div>

      {view === "chats" ? (
        <nav className="chat-list" aria-label={t("conversations")}>
          {rows.length === 0 ? (
            <p className="chat-empty">{t("noConversations")}</p>
          ) : (
            <ul>
              {rows.map((row) => {
                const selected = page === "conversation" && row.id === openThreadId;
                return (
                  <li key={row.id}>
                    <button
                      type="button"
                      className={selected ? "chat-row selected" : "chat-row"}
                      aria-current={selected ? "page" : undefined}
                      draggable
                      onDragStart={(event) => {
                        event.dataTransfer.setData("text/plain", row.name ?? "");
                        event.dataTransfer.effectAllowed = "copy";
                      }}
                      onDragEnd={(event) => {
                        // Dropped outside this window: it opens in a window of its own, where it was dropped.
                        const outside = event.clientX < 0 || event.clientY < 0 || event.clientX > window.innerWidth || event.clientY > window.innerHeight;
                        if (outside) {
                          const known = event.screenX !== 0 || event.screenY !== 0;
                          onPopout(row.id, known ? { x: event.screenX, y: event.screenY } : null);
                        }
                      }}
                      onClick={() => { onOpenThread(row.id); }}
                    >
                      <Faces faces={row.faces} more={row.more} />
                      <span className="chat-text">
                        <span className="chat-name">{row.name ?? t("untitledThread")}</span>
                        <span className={row.waiting ? "chat-preview waiting" : row.preview ? "chat-preview" : "chat-preview muted"}>
                          {row.waiting ? `${t("waitingForYou")}${row.preview ? `: ${row.preview}` : ""}` : (row.preview ?? t("noMessagesYet"))}
                        </span>
                      </span>
                      {row.waiting || row.unread ? (
                        <span className={row.waiting ? "chat-dot waiting" : "chat-dot unread"}>
                          <span className="sr-only">{row.waiting ? t("waitingForYou") : t("unread")}</span>
                        </span>
                      ) : null}
                    </button>
                  </li>
                );
              })}
            </ul>
          )}
        </nav>
      ) : (
        <nav className="surface-list" aria-label={t("surfaces")}>
          <ul>
            {DESKTOP_SURFACES.map((entry) => {
              const opens = OPENS[entry.id];
              // Several entries open the same page; only the page's own entry is marked current.
              const isCurrent = opens === page && entry.id === PAGE_ENTRY[page];
              const Icon = SURFACE_ICONS[entry.id];
              return (
                <li key={entry.id} className={isCurrent ? "selected" : undefined}>
                  {opens ? (
                    <button type="button" aria-current={isCurrent ? "page" : undefined} onClick={() => { onOpenPage(opens); }}>
                      <Icon />
                      {t(SURFACE_LABELS[entry.id])}
                    </button>
                  ) : (
                    <span className="nav-note">
                      <Icon />
                      {t(SURFACE_LABELS[entry.id])}
                    </span>
                  )}
                </li>
              );
            })}
          </ul>
        </nav>
      )}

      <div className="sidebar-foot">
        <div className="menu-anchor" ref={anchor}>
          <button
            type="button"
            className="account-button"
            aria-label={t("account")}
            aria-haspopup="menu"
            aria-expanded={accountOpen}
            onClick={() => { setAccountOpen(!accountOpen); }}
          >
            <Initials text={initials} />
          </button>
          {accountOpen ? (
            <div className="menu menu-up" role="menu" aria-label={t("account")}>
              <button
                type="button"
                role="menuitem"
                autoFocus
                onClick={() => {
                  setAccountOpen(false);
                  onOpenPage("settings");
                }}
              >
                <GearIcon />
                {t("settings")}
              </button>
              <hr />
              <button type="button" role="menuitem" onClick={onSignOut}>
                <LeaveIcon />
                {t("signOut")}
              </button>
            </div>
          ) : null}
        </div>
        <button type="button" className="pill-button" onClick={() => { onView(view === "chats" ? "workspace" : "chats"); }}>
          {view === "chats" ? <GridIcon /> : <ChatIcon />}
          {view === "chats" ? t("workspace") : t("chats")}
        </button>
      </div>
    </aside>
  );
}

