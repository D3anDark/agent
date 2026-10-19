export function parseAmount(raw: string): number {
  const value = parseFloat(raw);
  return Math.floor(value * 100);
}

export function formatCents(cents: number): string {
  return (cents / 100).toFixed(2);
}
