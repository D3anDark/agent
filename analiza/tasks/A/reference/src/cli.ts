import { formatCents } from "./money";
import { summarize } from "./report";
const path = process.argv[2];
if (!path) { console.error("usage: bun src/cli.ts <file.csv>"); process.exit(2); }
let text: string;
try { text = await Bun.file(path).text(); } catch { console.error(`error: cannot read ${path}`); process.exit(1); }
const { totals, skipped } = summarize(text);
const entries = [...totals].sort((a, b) => b[1] - a[1] || (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0));
let total = 0;
for (const [c, v] of entries) { console.log(`${c}\t${formatCents(v)}`); total += v; }
console.log(`TOTAL\t${formatCents(total)}`);
if (skipped) console.error(`skipped: ${skipped}`);
