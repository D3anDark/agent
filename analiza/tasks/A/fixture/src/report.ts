import { parseCsv } from "./csv";
import { parseAmount } from "./money";

export type Summary = {
  totals: Map<string, number>;
  skipped: number;
};

export function summarize(csvText: string): Summary {
  const rows = parseCsv(csvText).slice(1);
  const totals = new Map<string, number>();
  for (const [, category, , amount] of rows) {
    totals.set(category, (totals.get(category) ?? 0) + parseAmount(amount));
  }
  return { totals, skipped: 0 };
}
