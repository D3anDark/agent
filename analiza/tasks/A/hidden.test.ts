import { describe, expect, test } from "bun:test";
import { join } from "node:path";
import { parseCsv } from "../src/csv";
import { parseAmount } from "../src/money";
import { summarize } from "../src/report";

const root = join(import.meta.dir, "..");

describe("parseCsv", () => {
  test("escaped quotes", () => {
    expect(parseCsv('a,"say ""hi""",c\n')).toEqual([["a", 'say "hi"', "c"]]);
  });
  test("LF inside quotes", () => {
    expect(parseCsv('a,"x\ny",c\n')).toEqual([["a", "x\ny", "c"]]);
  });
  test("CRLF line endings", () => {
    expect(parseCsv("a,b\r\nc,d\r\n")).toEqual([["a", "b"], ["c", "d"]]);
  });
  test("CRLF inside quotes stays literal", () => {
    expect(parseCsv('a,"x\r\ny"\r\n')).toEqual([["a", "x\r\ny"]]);
  });
  test("no trailing newline", () => {
    expect(parseCsv("a,b\nc,d")).toEqual([["a", "b"], ["c", "d"]]);
  });
  test("empty fields", () => {
    expect(parseCsv("a,,c,\n")).toEqual([["a", "", "c", ""]]);
  });
  test("quoted empty field", () => {
    expect(parseCsv('a,"",c\n')).toEqual([["a", "", "c"]]);
  });
});

describe("parseAmount", () => {
  test.each([
    ["12", 1200],
    ["12.5", 1250],
    ["0.29", 29],
    ["-0.29", -29],
    ["1,234.50", 123450],
    ["1,000,000", 100000000],
    ["0.07", 7],
    ["1.15", 115],
    ["4.35", 435],
    ["8.20", 820],
    ["-0.05", -5],
  ])("%p", (raw, cents) => {
    expect(parseAmount(raw)).toBe(cents);
  });
  test.each(["", "abc", "1.234", "1.2.3", "12,34", "+5", ".5", "-", "1,2345", "12a"])(
    "rejects %p",
    (raw) => {
      expect(() => parseAmount(raw)).toThrow();
    },
  );
});

describe("summarize", () => {
  test("float-sensitive sums are exact", () => {
    const rows = ["date,category,description,amount"];
    for (let i = 0; i < 100; i++) rows.push(`2026-01-01,x,item,0.01`);
    rows.push("2026-01-01,y,item,1.15", "2026-01-01,y,item,4.35");
    const { totals } = summarize(rows.join("\n") + "\n");
    expect(totals.get("x")).toBe(100);
    expect(totals.get("y")).toBe(550);
  });
  test("trims category, case-sensitive", () => {
    const csv = "date,category,description,amount\nd, Food ,a,1\nd,Food,b,2\nd,food,c,3\n";
    const { totals } = summarize(csv);
    expect(totals.get("Food")).toBe(300);
    expect(totals.get("food")).toBe(300);
    expect(totals.size).toBe(2);
  });
  test("skips bad rows, ignores blank lines", () => {
    const csv =
      "date,category,description,amount\r\n" +
      "d,a,ok,1.00\r\n" +
      "\r\n" +
      "d,a,too,few\r\n" +
      "d,a,bad,1.234\r\n" +
      "d,a,x,1,extra\r\n" +
      "\n" +
      'd,b,"multi\nline, with comma","1,000.01"\r\n';
    const result = summarize(csv);
    expect(result.skipped).toBe(3);
    expect(result.totals.get("a")).toBe(100);
    expect(result.totals.get("b")).toBe(100001);
  });
  test("header only", () => {
    const result = summarize("date,category,description,amount\n");
    expect(result.totals.size).toBe(0);
    expect(result.skipped).toBe(0);
  });
});

async function cli(...args: string[]) {
  const proc = Bun.spawn(["bun", "src/cli.ts", ...args], { cwd: root, stdout: "pipe", stderr: "pipe" });
  const [stdout, stderr, code] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
    proc.exited,
  ]);
  return { stdout, stderr, code };
}

describe("cli", () => {
  test("sample report", async () => {
    const { stdout, stderr, code } = await cli("data/sample.csv");
    expect(code).toBe(0);
    expect(stdout).toBe("travel\t1234.50\nfun\t45.00\nfood\t2.81\nTOTAL\t1282.31\n");
    expect(stderr).toBe("");
  });
  test("ties by name, negative totals, skipped count", async () => {
    const path = "/tmp/hb-hidden-cli.csv";
    await Bun.write(
      path,
      "date,category,description,amount\nd,zeta,a,5\nd,alpha,b,5\nd,refunds,c,-0.05\nd,bad,d,oops\nd,short,e\n",
    );
    const { stdout, stderr, code } = await cli(path);
    expect(code).toBe(0);
    expect(stdout).toBe("alpha\t5.00\nzeta\t5.00\nrefunds\t-0.05\nTOTAL\t9.95\n");
    expect(stderr.trim()).toBe("skipped: 2");
  });
  test("missing file", async () => {
    const { stderr, code } = await cli("/tmp/definitely-missing-hb.csv");
    expect(code).toBe(1);
    expect(stderr.trim()).toBe("error: cannot read /tmp/definitely-missing-hb.csv");
  });
  test("no argument", async () => {
    const { stderr, code } = await cli();
    expect(code).toBe(2);
    expect(stderr.trim()).toBe("usage: bun src/cli.ts <file.csv>");
  });
});
