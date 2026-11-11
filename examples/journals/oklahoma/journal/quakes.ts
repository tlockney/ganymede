// Loaders for the Oklahoma earthquake journal. The notebook imports these
// so every section reads the catalog the same way.
//
// Catalog: USGS ComCat, fdsnws/event/1 CSV export (see the recorded curl
// cells in the notebook for the exact queries).
import { readFileSync } from "node:fs";
import * as pl from "npm:nodejs-polars@0.18.0";

/** Split one CSV line, honouring double quotes (the `place` column has commas). */
function splitCSV(line: string): string[] {
  const out: string[] = [];
  let cur = "";
  let quoted = false;
  for (const ch of line) {
    if (ch === '"') quoted = !quoted;
    else if (ch === "," && !quoted) {
      out.push(cur);
      cur = "";
    } else cur += ch;
  }
  out.push(cur);
  return out;
}

/** One row per event: time, year, lat, lon, depth, mag, magType, net, type, id, place. */
export function catalog(path = "../data/ok-m25.csv") {
  const [header, ...lines] = readFileSync(path, "utf8").trim().split("\n");
  const cols = splitCSV(header);
  const at = (name: string) => cols.indexOf(name);
  const rows = lines.map((line) => {
    const f = splitCSV(line);
    const time = f[at("time")];
    return {
      time,
      year: Number(time.slice(0, 4)),
      lat: Number(f[at("latitude")]),
      lon: Number(f[at("longitude")]),
      depth: Number(f[at("depth")]),
      mag: Number(f[at("mag")]),
      magType: f[at("magType")],
      net: f[at("net")],
      type: f[at("type")],
      id: f[at("id")],
      place: f[at("place")],
    };
  });
  return pl.DataFrame(rows);
}

/** Great-circle distance in km. */
export function km(lat1: number, lon1: number, lat2: number, lon2: number) {
  const r = Math.PI / 180;
  const a =
    Math.sin(((lat2 - lat1) * r) / 2) ** 2 +
    Math.cos(lat1 * r) * Math.cos(lat2 * r) * Math.sin(((lon2 - lon1) * r) / 2) ** 2;
  return 2 * 6371 * Math.asin(Math.sqrt(a));
}

type Event = { time: string; lat: number; lon: number; mag: number; magType: string; year: number };

/**
 * Pair each event in `a` with the event in `b` that is the same quake: origin
 * times within `seconds` and epicentres within `maxKm`. Two networks locate
 * the same quake a few seconds and kilometres apart. Both lists sorted by time.
 */
export function match(a: Event[], b: Event[], seconds = 8, maxKm = 15) {
  const tb = b.map((e) => Date.parse(e.time));
  const pairs: { year: number; time: string; magA: number; magB: number; typeA: string; typeB: string }[] = [];
  let lo = 0;
  for (const e of a) {
    const t = Date.parse(e.time);
    while (lo < tb.length && tb[lo] < t - seconds * 1000) lo++;
    for (let i = lo; i < tb.length && tb[i] <= t + seconds * 1000; i++) {
      if (km(e.lat, e.lon, b[i].lat, b[i].lon) <= maxKm) {
        pairs.push({ year: e.year, time: e.time, magA: e.mag, magB: b[i].mag, typeA: e.magType, typeB: b[i].magType });
        break;
      }
    }
  }
  return pairs;
}

/**
 * Gardner–Knopoff (1974) space-time windows: a quake of magnitude m claims the
 * smaller quakes within d km and t days of it as its aftershocks (and foreshocks).
 */
export function gkWindow(m: number) {
  const d = 10 ** (0.1238 * m + 0.983);
  const t = m >= 6.5 ? 10 ** (0.032 * m + 2.7389) : 10 ** (0.5409 * m - 0.547);
  return { km: d, days: t };
}

/**
 * Mark each event as a mainshock or a dependent (aftershock or foreshock),
 * largest first: an event inside the window of a larger, not-yet-claimed
 * event is dependent. Returns one boolean per event, in input order.
 * Events must be sorted by time.
 */
export function decluster(events: { time: string; lat: number; lon: number; mag: number }[]) {
  const t = events.map((e) => Date.parse(e.time));
  const dependent = new Array(events.length).fill(false);
  const order = events.map((_, i) => i).sort((a, b) => events[b].mag - events[a].mag);
  const DAY = 86_400_000;
  for (const i of order) {
    if (dependent[i]) continue;
    const w = gkWindow(events[i].mag);
    // Binary search for the window's start, then walk forward.
    let lo = 0, hi = t.length;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (t[mid] < t[i] - w.days * DAY) lo = mid + 1;
      else hi = mid;
    }
    for (let j = lo; j < t.length && t[j] <= t[i] + w.days * DAY; j++) {
      if (j === i || dependent[j] || events[j].mag > events[i].mag) continue;
      if (km(events[i].lat, events[i].lon, events[j].lat, events[j].lon) <= w.km) dependent[j] = true;
    }
  }
  return dependent;
}

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/**
 * OCC Form 1012 injection reports, one row per well and month with a volume:
 * year, month, api, lat, lon, formation, arbuckle (injects into the Arbuckle
 * group), barrels. The formation column is `FormationName` through 2024 and
 * `PermitFormationName` in 2025. Rows with no location are kept (lat/lon null).
 */
export async function injection(years: number[], dir = "../data/occ") {
  const XLSX = await import("npm:xlsx@0.18.5");
  const rows: Record<string, unknown>[] = [];
  for (const year of years) {
    const wb = XLSX.read(readFileSync(`${dir}/${year}.xlsx`));
    const sheet = XLSX.utils.sheet_to_json(wb.Sheets[wb.SheetNames[0]], { defval: null }) as Record<string, unknown>[];
    for (const r of sheet) {
      const formation = String(r.FormationName ?? r.PermitFormationName ?? "").trim().toUpperCase();
      MONTHS.forEach((m, i) => {
        const v = Number(r[`${m} Vol`]);
        if (!Number.isFinite(v) || v <= 0) return;
        rows.push({
          year, month: i + 1, api: String(r.API ?? ""),
          lat: r.LAT == null ? null : Number(r.LAT), lon: r.LON == null ? null : Number(r.LON),
          formation, arbuckle: formation.includes("ARBUCKLE"), barrels: v,
        });
      });
    }
  }
  return pl.DataFrame(rows);
}

/** A Polars frame as a Markdown table, numbers rounded. */
export function md(df: pl.DataFrame, digits = 0) {
  const cols = df.columns;
  const fmt = (v: unknown, c: string) =>
    typeof v === "number" && c !== "year"
      ? v.toLocaleString("en-US", { maximumFractionDigits: digits, minimumFractionDigits: digits })
      : String(v);
  const head = `| ${cols.join(" | ")} |\n| ${cols.map(() => "---").join(" | ")} |`;
  const body = df.toRecords().map((r) => `| ${cols.map((c) => fmt(r[c], c)).join(" | ")} |`).join("\n");
  return jupyter.md`${head}\n${body}`;
}
