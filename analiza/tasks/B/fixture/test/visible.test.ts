import { expect, test } from "bun:test";
import { maxSatisfying, satisfies } from "../src/semver";

test("caret", () => {
  expect(satisfies("1.4.0", "^1.2.3")).toBe(true);
  expect(satisfies("2.0.0", "^1.2.3")).toBe(false);
});

test("prerelease is excluded unless the comparator names the same tuple", () => {
  expect(satisfies("1.3.0-beta", "^1.2.3")).toBe(false);
  expect(satisfies("1.2.3-beta.2", ">=1.2.3-beta.1")).toBe(true);
});

test("maxSatisfying compares by precedence, not string order", () => {
  expect(maxSatisfying(["1.9.0", "1.10.0", "2.0.0"], "1.x")).toBe("1.10.0");
});
