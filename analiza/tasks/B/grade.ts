import { maxSatisfying, satisfies } from "./src/semver";
const { sat, max } = await Bun.file(new URL("./cases.json", import.meta.url)).json();
let ok = 0, n = 0; const fails: string[] = [];
for (const [v, r, want] of sat) { n++; let got; try { got = satisfies(v, r); } catch { got = "THROW"; } if (got === want) ok++; else fails.push(`sat ${v} | ${r} want ${want} got ${got}`); }
for (const [s, r, want] of max) { n++; let got; try { got = maxSatisfying([...s], r); } catch { got = "THROW"; } if (got === want) ok++; else fails.push(`max [${s}] | ${r} want ${want} got ${got}`); }
console.log(fails.slice(0, 40).join("\n")); console.log(`score ${ok}/${n}`);
