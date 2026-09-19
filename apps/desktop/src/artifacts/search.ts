/**
 * Global search (APP-008) over the typed `/v1/search` endpoint (DOMAIN.md §11.3): the
 * page builds a program from an enumeration of channels and a bounded budget; the query
 * text is data the server binds, never a predicate.
 */

export type SearchChannel = "artifacts" | "threads" | "knowledge" | "evidence";

export const SEARCH_CHANNELS: readonly SearchChannel[] = ["artifacts", "threads", "knowledge", "evidence"];

/** Longest query the page sends (the server's own bound). */
export const MAX_QUERY_CHARS = 200;
/** Largest per-channel result budget the page asks for. */
export const MAX_RESULTS = 50;

export interface SearchRequest {
  readonly q: string;
  readonly channels: readonly SearchChannel[];
  readonly maxResults: number;
}

export interface SearchHit {
  readonly channel: SearchChannel;
  readonly id: string;
  readonly title: string;
  readonly snippet: string;
  readonly matched: string;
  readonly score: number;
  readonly ref: Readonly<Record<string, string | null>>;
  readonly provenance: { readonly sourceId: string; readonly locator: string; readonly snapshot: string };
}

export interface SearchResults {
  readonly q: string;
  readonly channels: readonly SearchChannel[];
  readonly hits: readonly SearchHit[];
  readonly truncated: boolean;
  readonly engines: readonly { readonly channel: string; readonly engine: string }[];
}

/** Validate and normalize what the page asks for; throws on anything outside the program. */
export function searchRequest(q: string, channels: readonly string[] = SEARCH_CHANNELS, maxResults = 20): SearchRequest {
  const trimmed = q.trim();
  if (!trimmed || trimmed.length > MAX_QUERY_CHARS) {
    throw new Error(`query must be 1..=${String(MAX_QUERY_CHARS)} characters`);
  }
  const chosen: SearchChannel[] = [];
  for (const channel of channels) {
    if (!(SEARCH_CHANNELS as readonly string[]).includes(channel)) {
      throw new Error(`${channel} is not a search channel`);
    }
    if (!chosen.includes(channel as SearchChannel)) {
      chosen.push(channel as SearchChannel);
    }
  }
  if (chosen.length === 0) {
    throw new Error("at least one channel is needed");
  }
  if (!Number.isInteger(maxResults) || maxResults < 1 || maxResults > MAX_RESULTS) {
    throw new Error(`maxResults must be 1..=${String(MAX_RESULTS)}`);
  }
  return { q: trimmed, channels: chosen, maxResults };
}

/** The query string for `GET /v1/search`. */
export function searchQueryString(workspaceId: string, request: SearchRequest): string {
  const params = new URLSearchParams();
  params.set("workspace_id", workspaceId);
  params.set("q", request.q);
  params.set("channels", request.channels.join(","));
  params.set("max_results", String(request.maxResults));
  return params.toString();
}

function text(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : {};
}

/** Decode the endpoint's response. */
export function decodeSearch(payload: unknown): SearchResults {
  const root = record(payload);
  const program = record(root["program"]);
  const hits: SearchHit[] = [];
  for (const raw of Array.isArray(root["hits"]) ? (root["hits"] as unknown[]) : []) {
    const hit = record(raw);
    const channel = text(hit["channel"]);
    const id = text(hit["id"]);
    if (!id || !channel || !(SEARCH_CHANNELS as readonly string[]).includes(channel)) {
      continue;
    }
    const ref: Record<string, string | null> = {};
    for (const [key, value] of Object.entries(record(hit["ref"]))) {
      ref[key] = text(value);
    }
    const provenance = record(hit["provenance"]);
    hits.push({
      channel: channel as SearchChannel,
      id,
      title: text(hit["title"]) ?? id,
      snippet: text(hit["snippet"]) ?? "",
      matched: text(hit["matched"]) ?? "",
      score: typeof hit["score"] === "number" ? hit["score"] : 0,
      ref,
      provenance: {
        sourceId: text(provenance["source_id"]) ?? id,
        locator: text(provenance["locator"]) ?? "",
        snapshot: text(provenance["snapshot"]) ?? "",
      },
    });
  }
  const channels = (Array.isArray(program["channels"]) ? (program["channels"] as unknown[]) : [])
    .filter((item): item is SearchChannel => typeof item === "string" && (SEARCH_CHANNELS as readonly string[]).includes(item));
  const engines = (Array.isArray(root["engines"]) ? (root["engines"] as unknown[]) : []).map((raw) => {
    const engine = record(raw);
    return { channel: text(engine["channel"]) ?? "", engine: text(engine["engine"]) ?? "" };
  });
  return { q: text(program["q"]) ?? "", channels, hits, truncated: root["truncated"] === true, engines };
}
