import { expect, test } from "bun:test";
import { parseCsv } from "../src/csv";
import { parseAmount } from "../src/money";
import { summarize } from "../src/report";

test("parses quoted fields with commas", () => {
  expect(parseCsv('a,"b,c",d\n')).toEqual([["a", "b,c", "d"]]);
});

test("amounts are exact cents", () => {
  expect(parseAmount("0.29")).toBe(29);
  expect(parseAmount("-12.5")).toBe(-1250);
});

test("summarizes per category", () => {
  const csv = "date,category,description,amount\n2026-01-01,food,bread,3.10\n2026-01-02,food,milk,0.29\n";
  expect(summarize(csv).totals.get("food")).toBe(339);
});
