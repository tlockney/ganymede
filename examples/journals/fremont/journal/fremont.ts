// Loaders for the Fremont Bridge journal. The notebook imports these so
// every section reads the data the same way.
//
// Counter: Seattle Open Data 65db-xm6k, hourly bike counts on the Fremont
// Bridge sidewalks. Weather: NOAA GHCN-Daily, SeaTac (USW00024233).
import { readFileSync } from "node:fs";
import * as pl from "npm:nodejs-polars@0.18.0";

const WEEKDAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

/** "10/02/2012 01:00:00 PM" -> its parts. The counter's times are local. */
function parseStamp(s: string) {
  const [d, t, ampm] = s.split(" ");
  const [mo, da, yr] = d.split("/").map(Number);
  let h = Number(t.slice(0, 2)) % 12;
  if (ampm === "PM") h += 12;
  const day = `${yr}-${String(mo).padStart(2, "0")}-${String(da).padStart(2, "0")}`;
  const dow = (new Date(`${day}T12:00:00Z`).getUTCDay() + 6) % 7; // Mon = 0
  return { day, year: yr, month: mo, hour: h, dow };
}

/** One row per hour: day, year, month, hour, dow, weekday, total, west, east. Hours with no count are dropped. */
export function hourly(path = "../data/fremont.csv") {
  const lines = readFileSync(path, "utf8").trim().split("\n").slice(1);
  const rows = [];
  for (const line of lines) {
    const [stamp, total, west, east] = line.split(",");
    if (total === "") continue;
    const p = parseStamp(stamp);
    rows.push({
      ...p,
      weekday: WEEKDAYS[p.dow],
      workday: p.dow < 5,
      total: Number(total),
      west: Number(west),
      east: Number(east),
    });
  }
  return pl.DataFrame(rows);
}

/** One row per day: day, year, month, dow, weekday, workday, total. */
export function daily(h: pl.DataFrame) {
  return h
    .groupBy(["day", "year", "month", "dow", "weekday", "workday"])
    .agg(pl.col("total").sum().alias("total"))
    .sort("day");
}

/** SeaTac daily rain (mm) and high (°C), keyed by day. */
export function weather(path = "../data/seatac.csv") {
  const lines = readFileSync(path, "utf8").trim().split("\n").slice(1);
  const rows = lines.map((l) => {
    const [, day, prcp, tmax] = l.split(",").map((v) => v.replaceAll('"', ""));
    return { day, rain_mm: Number(prcp), tmax_c: Number(tmax) };
  });
  return pl.DataFrame(rows);
}

/** A Polars frame as a Markdown table, numbers rounded. */
export function md(df: pl.DataFrame, digits = 0) {
  const cols = df.columns;
  const fmt = (v: unknown, c: string) =>
    typeof v === "number" && c !== "year" ? v.toLocaleString("en-US", { maximumFractionDigits: digits, minimumFractionDigits: digits }) : String(v);
  const head = `| ${cols.join(" | ")} |\n| ${cols.map(() => "---").join(" | ")} |`;
  const body = df.toRecords().map((r) => `| ${cols.map((c) => fmt(r[c], c)).join(" | ")} |`).join("\n");
  return jupyter.md`${head}\n${body}`;
}
