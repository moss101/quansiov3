/**
 * The floating activity indicator (ADP-004, DOMAIN.md §13.6 C).
 *
 * The window is transparent, always on top and click-through: the pointer passes through it to whatever is underneath
 * everywhere except the anchor and the list, which tell main when the pointer is over them (`setIndicatorInteractive`)
 * so only they receive mouse events. The list is what main derived from durable events, ordered waiting for you,
 * running, errors, completed; "Open" asks main to bring the main window forward on the item, and "Dismiss" removes it
 * from the list (a preference of this app; nothing on the server changes). The anchor can be dragged: main clamps the
 * position to a display and remembers it per display.
 */

import { useCallback, useEffect, useRef, useState, type PointerEvent as ReactPointerEvent, type ReactElement } from "react";

import { t } from "../i18n/strings.js";
import { decodeActivity, groupActivity, isRemoteRow, remoteAnswers, GROUP_HEADINGS, REMOTE_ANSWER_LABELS, type ActivityRow } from "../shell-ui.js";
import { CloseIcon, PulseIcon } from "./icons.js";

/** The owner's answers on a remote-control row: allow, deny, or Stop. A row that is not a remote session has none. */
export function RemoteAnswers({ row }: { readonly row: ActivityRow }): ReactElement | null {
  const answers = remoteAnswers(row.kind);
  if (answers.length === 0) {
    return null;
  }
  return (
    <>
      {answers.map((answer) => (
        <button
          key={answer}
          type="button"
          className={answer === "stop" || answer === "deny" ? "link danger" : "link"}
          onClick={() => {
            void window.quansio.answerRemote(row.key, answer);
          }}
        >
          {t(REMOTE_ANSWER_LABELS[answer])}
        </button>
      ))}
    </>
  );
}

export function Indicator(): ReactElement {
  const [rows, setRows] = useState<readonly ActivityRow[]>([]);
  const [collapsed, setCollapsed] = useState(false);
  const grab = useRef<{ x: number; y: number } | null>(null);
  const interactive = useRef(false);

  useEffect(() => {
    const off = window.quansio.onActivity((payload) => {
      setRows(decodeActivity(payload));
    });
    void window.quansio.activityItems().then((answer) => {
      setRows(decodeActivity(answer));
    });
    void window.quansio.consumePending();
    return off;
  }, []);

  const setInteractive = useCallback((active: boolean) => {
    if (interactive.current !== active) {
      interactive.current = active;
      void window.quansio.setIndicatorInteractive(active);
    }
  }, []);

  const hover = {
    onPointerEnter: () => {
      setInteractive(true);
    },
    onPointerLeave: () => {
      // While the anchor is being dragged the pointer stays "inside" however fast the window follows it.
      if (grab.current === null) {
        setInteractive(false);
      }
    },
  };

  const onPointerDown = (event: ReactPointerEvent<HTMLButtonElement>): void => {
    grab.current = { x: event.clientX, y: event.clientY };
    event.currentTarget.setPointerCapture(event.pointerId);
  };
  const onPointerMove = (event: ReactPointerEvent<HTMLButtonElement>): void => {
    const origin = grab.current;
    if (origin !== null && event.buttons === 1) {
      void window.quansio.moveIndicator(Math.round(event.screenX - origin.x), Math.round(event.screenY - origin.y));
    }
  };
  const onPointerUp = (event: ReactPointerEvent<HTMLButtonElement>): void => {
    const moved = grab.current !== null && (Math.abs(event.clientX - grab.current.x) > 3 || Math.abs(event.clientY - grab.current.y) > 3);
    grab.current = null;
    event.currentTarget.releasePointerCapture(event.pointerId);
    if (!moved) {
      setCollapsed((value) => !value);
    }
  };

  const groups = groupActivity(rows);
  const waiting = rows.filter((row) => row.group === "waiting").length;

  return (
    <main className="indicator" aria-label={t("indicatorTitle")}>
      <button
        type="button"
        className={waiting > 0 ? "indicator-anchor waiting" : "indicator-anchor"}
        aria-label={t("indicatorTitle")}
        aria-expanded={!collapsed}
        {...hover}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
      >
        <PulseIcon />
        <span className="indicator-count">{rows.length}</span>
      </button>
      {collapsed ? null : (
        <section className="indicator-list" {...hover}>
          {groups.length === 0 ? (
            <p className="indicator-empty">{t("indicatorEmpty")}</p>
          ) : (
            groups.map((entry) => (
              <div key={entry.group} className="indicator-group">
                <h2>{t(GROUP_HEADINGS[entry.group])}</h2>
                <ul>
                  {entry.rows.map((row) => (
                    <li key={row.key} className={`indicator-item ${row.group}`}>
                      <span className="indicator-title">{row.title}</span>
                      {isRemoteRow(row.kind) ? (
                        // A computer being watched or controlled is shown until the session ends: answered or stopped, never dismissed.
                        <RemoteAnswers row={row} />
                      ) : (
                        <>
                          <button
                            type="button"
                            className="link"
                            onClick={() => {
                              void window.quansio.openActivity(row.key);
                            }}
                          >
                            {t("indicatorOpen")}
                          </button>
                          <button
                            type="button"
                            className="icon-button ghost tiny"
                            aria-label={t("indicatorDismiss")}
                            data-tip={t("indicatorDismiss")}
                            onClick={() => {
                              void window.quansio.dismissActivity(row.key);
                            }}
                          >
                            <CloseIcon size={12} />
                          </button>
                        </>
                      )}
                    </li>
                  ))}
                </ul>
              </div>
            ))
          )}
        </section>
      )}
    </main>
  );
}
