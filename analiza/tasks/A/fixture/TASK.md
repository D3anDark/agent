# Ledger report

`src/` holds a tiny expense ledger tool. It is half done and has bugs.

## Input

A CSV file with a header row `date,category,description,amount`.

- Fields follow RFC 4180: a field may be wrapped in double quotes; inside quotes a
  comma, a line break (LF or CRLF) and an escaped quote (`""`) are literal.
- Lines end with LF or CRLF. A trailing line break at the end of the file is allowed.
- `amount` matches the regex `^-?(\d+|\d{1,3}(,\d{3})+)(\.\d{1,2})?$`: an optional
  leading `-` (a refund), an integer part that may use `,` thousands separators in
  groups of three (such a field is quoted in the CSV, for example `"1,234.50"`), and
  at most 2 fraction digits. Examples: `12`, `12.5`, `-0.29`, `1,234.50`, `1,000,000`.
- Category names are trimmed of surrounding spaces and compared case-sensitively.

## Rules

1. `parseCsv` in `src/csv.ts` must implement the format above.
2. Money is exact: `parseAmount` in `src/money.ts` returns integer cents, never a float
   that went through `* 100`. A string that does not match the amount format (for
   example empty, `abc`, `1.234`, `1.2.3`, `12,34`, `+5`, `.5`) makes `parseAmount` throw.
3. `summarize` in `src/report.ts` returns per-category totals in cents. A data row
   with the wrong number of columns or an invalid amount is skipped and counted in
   `skipped`. Blank lines are ignored and not counted.
4. Add a CLI, `bun src/cli.ts <file.csv>`, that prints to stdout one line per category:
   `<category>\t<total>` where total is formatted with exactly 2 decimals and a `-` sign
   for negative totals (`-0.05`). Sort by total descending, ties by category name
   ascending. The last line is `TOTAL\t<grand total>`. If any rows were skipped, print
   `skipped: <N>` to stderr. Exit code 0.
   A missing file prints `error: cannot read <path>` to stderr and exits with code 1.
   No argument prints `usage: bun src/cli.ts <file.csv>` to stderr and exits with code 2.

Keep the existing exported function names and signatures.
