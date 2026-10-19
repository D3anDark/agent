export function satisfies(version: string, range: string): boolean {
  return version === range;
}

export function maxSatisfying(versions: string[], range: string): string | null {
  const matching = versions.filter((v) => satisfies(v, range));
  return matching.sort().at(-1) ?? null;
}
