/**
 * The command palette (APP-012): one field that finds a page, a workspace or an action and opens
 * it (Cmd/Ctrl+K or the search button). It only navigates the page — every item's `run` is
 * something the shell already does from a click — so it adds a way in, never a new command, and
 * it offers only what the navigation offers.
 */

import { useEffect, useMemo, useRef, useState, type ReactElement, type ReactNode } from "react";

import { SearchIcon } from "./icons.js";
import { t } from "./strings.js";

export interface PaletteItem {
  readonly key: string;
  readonly group: string;
  readonly label: string;
  readonly detail?: string | null;
  readonly icon: ReactNode;
  readonly run: () => void;
}

/** Items whose label or detail contains every word typed, in their original order. */
export function matchItems(items: readonly PaletteItem[], query: string): readonly PaletteItem[] {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (words.length === 0) {
    return items;
  }
  return items.filter((item) => {
    const haystack = `${item.label} ${item.detail ?? ""}`.toLowerCase();
    return words.every((word) => haystack.includes(word));
  });
}

export function Palette({ items, onClose }: { readonly items: readonly PaletteItem[]; readonly onClose: () => void }): ReactElement {
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const list = useRef<HTMLUListElement | null>(null);
  const shown = useMemo(() => matchItems(items, query), [items, query]);
  const current = Math.min(active, Math.max(0, shown.length - 1));

  useEffect(() => {
    list.current?.querySelector<HTMLElement>(`[data-index="${String(current)}"]`)?.scrollIntoView({ block: "nearest" });
  }, [current]);

  const choose = (item: PaletteItem | undefined): void => {
    if (item) {
      onClose();
      item.run();
    }
  };

  let lastGroup = "";
  return (
    <>
      <div className="scrim palette-scrim" aria-hidden="true" onMouseDown={onClose} />
      <div className="palette" role="dialog" aria-modal="true" aria-label={t("paletteTitle")}>
        <div className="palette-field">
          <SearchIcon size={18} />
          <input
            type="search"
            autoFocus
            role="combobox"
            aria-expanded="true"
            aria-controls="palette-list"
            aria-activedescendant={shown[current] ? `palette-${shown[current].key}` : undefined}
            aria-label={t("paletteTitle")}
            placeholder={t("palettePlaceholder")}
            value={query}
            onChange={(event) => {
              setQuery(event.target.value);
              setActive(0);
            }}
            onKeyDown={(event) => {
              if (event.key === "ArrowDown") {
                event.preventDefault();
                setActive(Math.min(current + 1, shown.length - 1));
              } else if (event.key === "ArrowUp") {
                event.preventDefault();
                setActive(Math.max(current - 1, 0));
              } else if (event.key === "Enter") {
                event.preventDefault();
                choose(shown[current]);
              } else if (event.key === "Escape") {
                event.preventDefault();
                onClose();
              }
            }}
          />
        </div>
        {shown.length === 0 ? (
          <p className="palette-empty">{t("paletteEmpty")}</p>
        ) : (
          <ul id="palette-list" role="listbox" aria-label={t("paletteTitle")} ref={list}>
            {shown.map((item, index) => {
              const heading = item.group !== lastGroup ? item.group : null;
              lastGroup = item.group;
              return (
                <li key={item.key} role="presentation">
                  {heading ? <span className="palette-group" role="presentation">{heading}</span> : null}
                  <div
                    id={`palette-${item.key}`}
                    role="option"
                    aria-selected={index === current}
                    data-index={index}
                    className="palette-row"
                    onMouseMove={() => { setActive(index); }}
                    onClick={() => { choose(item); }}
                  >
                    <span className="palette-icon">{item.icon}</span>
                    <span className="palette-label">{item.label}</span>
                    {item.detail ? <span className="palette-detail">{item.detail}</span> : null}
                  </div>
                </li>
              );
            })}
          </ul>
        )}
      </div>
    </>
  );
}
