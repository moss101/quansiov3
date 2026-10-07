/**
 * Time zones for cron triggers (ADP-007, DOMAIN.md §13.8 D).
 *
 * A schedule's windows are wall-clock times in an IANA zone the server resolves from its own
 * database; this module only offers names to pick from. The picker is a convenience: the server
 * decides whether a name is a zone (`AUTOMATION_TIMEZONE_UNKNOWN`) and nothing falls back to UTC.
 */

/** Used when the runtime cannot list zones. */
export const FALLBACK_ZONES: readonly string[] = [
  "UTC",
  "Africa/Cairo",
  "Africa/Johannesburg",
  "Africa/Lagos",
  "America/Anchorage",
  "America/Chicago",
  "America/Denver",
  "America/Los_Angeles",
  "America/Mexico_City",
  "America/New_York",
  "America/Sao_Paulo",
  "America/Toronto",
  "Asia/Dubai",
  "Asia/Hong_Kong",
  "Asia/Karachi",
  "Asia/Kolkata",
  "Asia/Seoul",
  "Asia/Shanghai",
  "Asia/Singapore",
  "Asia/Tokyo",
  "Australia/Sydney",
  "Europe/Berlin",
  "Europe/London",
  "Europe/Madrid",
  "Europe/Paris",
  "Europe/Rome",
  "Pacific/Auckland",
  "Pacific/Honolulu",
];

interface IntlWithZones {
  supportedValuesOf?: (key: string) => string[];
}

/** The IANA zone names the runtime knows, `UTC` first; a short list when it cannot say. */
export function supportedZones(intl: IntlWithZones = Intl as unknown as IntlWithZones): readonly string[] {
  try {
    const listed = intl.supportedValuesOf?.("timeZone");
    if (listed && listed.length > 0) {
      return ["UTC", ...listed.filter((zone) => zone !== "UTC")];
    }
  } catch {
    // The runtime does not support the key: use the short list.
  }
  return FALLBACK_ZONES;
}

/** The viewer's own zone, or `UTC` when the runtime does not say. */
export function viewerZone(): string {
  try {
    const zone = Intl.DateTimeFormat().resolvedOptions().timeZone;
    return zone.trim() === "" ? "UTC" : zone;
  } catch {
    return "UTC";
  }
}

function squash(text: string): string {
  return text.toLowerCase().replace(/[\s_/-]+/g, "");
}

/** Zones that match what was typed (case, spaces, underscores and slashes ignored), best matches first, at most `limit`. */
export function filterZones(zones: readonly string[], query: string, limit = 100): readonly string[] {
  const needle = squash(query);
  if (needle === "") {
    return zones.slice(0, limit);
  }
  const starts: string[] = [];
  const contains: string[] = [];
  for (const zone of zones) {
    const hay = squash(zone);
    if (hay.startsWith(needle)) {
      starts.push(zone);
    } else if (hay.includes(needle)) {
      contains.push(zone);
    }
  }
  return [...starts, ...contains].slice(0, limit);
}

/** A fixed offset the server accepts: `±HH:MM`. */
export function isFixedOffset(value: string): boolean {
  return /^[+-](0\d|1[0-4]):[0-5]\d$/.test(value.trim());
}

/** `+02:00` for 7200 seconds east of UTC. */
export function offsetLabel(seconds: number): string {
  const sign = seconds < 0 ? "-" : "+";
  const absolute = Math.abs(Math.trunc(seconds));
  const hours = Math.floor(absolute / 3600);
  const minutes = Math.floor((absolute % 3600) / 60);
  return `${sign}${String(hours).padStart(2, "0")}:${String(minutes).padStart(2, "0")}`;
}
