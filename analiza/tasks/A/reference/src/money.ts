export function parseAmount(raw: string): number {
  const m = /^(-?)(\d+|\d{1,3}(?:,\d{3})+)(?:\.(\d{1,2}))?$/.exec(raw);
  if (!m) throw new Error(`invalid amount: ${raw}`);
  const cents = Number(m[2].replaceAll(",", "")) * 100 + Number((m[3] ?? "").padEnd(2, "0"));
  return m[1] ? -cents : cents;
}
export function formatCents(cents: number): string {
  const abs = Math.abs(cents);
  return `${cents < 0 ? "-" : ""}${Math.floor(abs / 100)}.${String(abs % 100).padStart(2, "0")}`;
}
