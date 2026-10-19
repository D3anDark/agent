import { parseCsv } from "./csv";
import { parseAmount } from "./money";
export type Summary = { totals: Map<string, number>; skipped: number };
export function summarize(csvText: string): Summary {
  const rows = parseCsv(csvText).slice(1);
  const totals = new Map<string, number>();
  let skipped = 0;
  for (const row of rows) {
    if (row.length === 1 && row[0] === "") continue;
    if (row.length !== 4) { skipped++; continue; }
    let cents: number;
    try { cents = parseAmount(row[3]); } catch { skipped++; continue; }
    const category = row[1].trim();
    totals.set(category, (totals.get(category) ?? 0) + cents);
  }
  return { totals, skipped };
}
