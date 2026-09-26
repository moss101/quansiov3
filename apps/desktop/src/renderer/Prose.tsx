/**
 * Message text as a person reads it (APP-004): paragraphs, lists, headings, quotes, fenced code,
 * inline code, bold and links. Parsed into React elements — never HTML — so message content is
 * data on the page, not markup; only http(s) links become anchors (the main process opens them
 * in the system browser and never navigates the renderer).
 */

import type { ReactElement, ReactNode } from "react";

export type Block =
  | { readonly kind: "paragraph"; readonly text: string }
  | { readonly kind: "heading"; readonly level: 1 | 2 | 3; readonly text: string }
  | { readonly kind: "quote"; readonly text: string }
  | { readonly kind: "code"; readonly text: string }
  | { readonly kind: "list"; readonly ordered: boolean; readonly items: readonly string[] };

const BULLET = /^\s*(?:[-*•])\s+(.*)$/;
const NUMBERED = /^\s*\d+[.)]\s+(.*)$/;
const HEADING = /^(#{1,3})\s+(.*)$/;
const QUOTE = /^>\s?(.*)$/;
const FENCE = /^```/;

/** Split message text into blocks; anything unrecognised is a paragraph, kept as written. */
export function parseBlocks(source: string): readonly Block[] {
  const lines = source.replace(/\r\n?/g, "\n").split("\n");
  const blocks: Block[] = [];
  let paragraph: string[] = [];
  const flush = (): void => {
    if (paragraph.length > 0) {
      blocks.push({ kind: "paragraph", text: paragraph.join("\n") });
      paragraph = [];
    }
  };
  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index] ?? "";
    if (FENCE.test(line)) {
      flush();
      const code: string[] = [];
      index += 1;
      while (index < lines.length && !FENCE.test(lines[index] ?? "")) {
        code.push(lines[index] ?? "");
        index += 1;
      }
      blocks.push({ kind: "code", text: code.join("\n") });
      continue;
    }
    if (!line.trim()) {
      flush();
      continue;
    }
    const heading = HEADING.exec(line);
    if (heading) {
      flush();
      blocks.push({ kind: "heading", level: Math.min(3, heading[1]?.length ?? 1) as 1 | 2 | 3, text: heading[2] ?? "" });
      continue;
    }
    const bullet = BULLET.exec(line);
    const numbered = bullet ? null : NUMBERED.exec(line);
    if (bullet || numbered) {
      flush();
      const ordered = numbered !== null;
      const pattern = ordered ? NUMBERED : BULLET;
      const items: string[] = [];
      while (index < lines.length) {
        const match = pattern.exec(lines[index] ?? "");
        if (!match) {
          break;
        }
        items.push(match[1] ?? "");
        index += 1;
      }
      index -= 1;
      blocks.push({ kind: "list", ordered, items });
      continue;
    }
    const quote = QUOTE.exec(line);
    if (quote) {
      flush();
      const quoted: string[] = [];
      while (index < lines.length) {
        const match = QUOTE.exec(lines[index] ?? "");
        if (!match) {
          break;
        }
        quoted.push(match[1] ?? "");
        index += 1;
      }
      index -= 1;
      blocks.push({ kind: "quote", text: quoted.join("\n") });
      continue;
    }
    paragraph.push(line);
  }
  flush();
  return blocks;
}

export type Inline =
  | { readonly kind: "text"; readonly text: string }
  | { readonly kind: "code"; readonly text: string }
  | { readonly kind: "strong"; readonly text: string }
  | { readonly kind: "link"; readonly text: string; readonly href: string };

const INLINE = /(`[^`\n]+`)|(\*\*[^*\n]+\*\*)|(\[[^\]\n]+\]\((https?:\/\/[^)\s]+)\))|(https?:\/\/[^\s<>()]*[^\s<>().,;:!?'"])/g;

/** Split a run of text into inline spans. */
export function parseInline(source: string): readonly Inline[] {
  const spans: Inline[] = [];
  let last = 0;
  for (const match of source.matchAll(INLINE)) {
    const at = match.index;
    if (at > last) {
      spans.push({ kind: "text", text: source.slice(last, at) });
    }
    const [whole, code, strong, link, linkHref, bare] = match;
    if (code) {
      spans.push({ kind: "code", text: code.slice(1, -1) });
    } else if (strong) {
      spans.push({ kind: "strong", text: strong.slice(2, -2) });
    } else if (link && linkHref) {
      spans.push({ kind: "link", text: link.slice(1, link.indexOf("](")), href: linkHref });
    } else if (bare) {
      spans.push({ kind: "link", text: bare, href: bare });
    }
    last = at + whole.length;
  }
  if (last < source.length) {
    spans.push({ kind: "text", text: source.slice(last) });
  }
  return spans;
}

function InlineText({ text }: { readonly text: string }): ReactElement {
  return (
    <>
      {parseInline(text).map((span, index): ReactNode => {
        const key = `${span.kind}-${String(index)}`;
        switch (span.kind) {
          case "code":
            return <code key={key}>{span.text}</code>;
          case "strong":
            return <strong key={key}>{span.text}</strong>;
          case "link":
            return (
              <a key={key} href={span.href} target="_blank" rel="noreferrer noopener">
                {span.text}
              </a>
            );
          default:
            return span.text;
        }
      })}
    </>
  );
}

export function Prose({ text }: { readonly text: string }): ReactElement {
  return (
    <div className="prose">
      {parseBlocks(text).map((block, index): ReactNode => {
        const key = `${block.kind}-${String(index)}`;
        switch (block.kind) {
          case "heading":
            return block.level === 1 ? <h4 key={key} className="prose-h1"><InlineText text={block.text} /></h4> : <h5 key={key} className="prose-h2"><InlineText text={block.text} /></h5>;
          case "quote":
            return <blockquote key={key}><InlineText text={block.text} /></blockquote>;
          case "code":
            return <pre key={key}><code>{block.text}</code></pre>;
          case "list": {
            const items = block.items.map((item, itemIndex) => <li key={`${key}-${String(itemIndex)}`}><InlineText text={item} /></li>);
            return block.ordered ? <ol key={key}>{items}</ol> : <ul key={key}>{items}</ul>;
          }
          default:
            return <p key={key}><InlineText text={block.text} /></p>;
        }
      })}
    </div>
  );
}
