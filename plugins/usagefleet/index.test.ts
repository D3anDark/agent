import { describe, expect, test } from "bun:test";
import { accountLabels, assignToken, blockMessage, claudeAccount, formatGroupLimits, formatLimits, resolveToken, tokenReport, type Context } from "./index.ts";

const now = Date.parse("2026-10-15T12:00:00Z");
const context: Context = { provider: "anthropic", model: "claude-opus-5-5", accountId: "uuid-2", accountName: "home", cwd: "/", sessionId: "s" };

describe("usagefleet", () => {
	test("shows fresh account limits and hides stale ones", () => {
		const limits = { fiveHourPct: 5.4, sevenDayPct: 41, at: "2026-10-15T11:52:00Z" };
		expect(formatLimits({ limits }, now)).toEqual([{ text: "5h " }, { text: "5%", tone: "ok" }, { text: " · 7d " }, { text: "41%", tone: "ok" }]);
		expect(formatLimits({ limits: { ...limits, at: "2026-10-15T11:40:00Z" } }, now)).toBeNull();
	});

	test("tones percents green below 50, orange below 80, red from 80", () => {
		const tones = (fiveHourPct: number, sevenDayPct: number) =>
			formatLimits({ limits: { fiveHourPct, sevenDayPct, at: "2026-10-15T11:58:00Z" } }, now)?.map((part) => part.tone);
		expect(tones(49.4, 49.5)).toEqual([undefined, "ok", undefined, "warn"]);
		expect(tones(79.4, 79.5)).toEqual([undefined, "warn", undefined, "error"]);
	});

	test("shows the group budget unless it repeats the account's", () => {
		const view = { group: "Artur", sessionPct: 30, weeklyPct: 17 };
		const account = formatLimits({ limits: { fiveHourPct: 5, sevenDayPct: 41, at: "2026-10-15T11:58:00Z" } }, now);
		expect(formatGroupLimits(view, account)?.map((part) => part.text).join("")).toBe("Artur: 5h 30% · 7d 17%");
		const same = formatLimits({ limits: { fiveHourPct: 30, sevenDayPct: 17, at: "2026-10-15T11:58:00Z" } }, now);
		expect(formatGroupLimits(view, same)).toBeNull();
	});

	test("picks the session account's token before the shared one", () => {
		const config = { token: "shared", accountTokens: { "uuid-1": "first", "uuid-2": "second" } };
		expect(resolveToken(config, "uuid-2", {})).toBe("second");
		expect(resolveToken(config, "uuid-9", {})).toBe("shared");
		expect(resolveToken({ accountTokens: { "uuid-1": "first" } }, "uuid-9", {})).toBe("first");
		expect(resolveToken(config, "uuid-2", { USAGEFLEET_TOKEN: "env" })).toBe("env");
	});

	test("assigns a token to one account and keeps the rest of the config", () => {
		const config = { token: "shared", state: { deviceId: "d" }, accountTokens: { "uuid-1": "first" } };
		expect(assignToken(config, "uuid-2", "uf_second")).toEqual({
			token: "shared",
			state: { deviceId: "d" },
			accountTokens: { "uuid-1": "first", "uuid-2": "uf_second" },
		});
	});

	test("labels each account with its assigned token, masked", () => {
		const config = { token: "uf_sharedtoken", accountTokens: { "uuid-1": "uf_firsttoken", "uuid-2": "" } };
		expect(accountLabels(config)).toEqual({ "uuid-1": "uf_firs…" });
		expect(accountLabels(null)).toEqual({});
	});

	test("reports which token the account uses without showing it", () => {
		const config = { token: "uf_sharedtoken", accountTokens: { "uuid-1": "uf_firsttoken" } };
		expect(tokenReport(config, "uuid-1", "work", {})).toBe("Account work uses its own token uf_firs….");
		expect(tokenReport(config, "uuid-2", "home", {})).toStartWith("Account home has no token of its own and uses uf_shar….");
		expect(tokenReport({}, "uuid-2", "home", {})).toStartWith("Account home has no usagefleet token.");
	});

	test("guards only Claude subscription accounts", () => {
		expect(claudeAccount(context)).toBe("uuid-2");
		expect(claudeAccount({ ...context, accountId: null })).toBeNull();
		expect(claudeAccount({ ...context, provider: "openai-codex" })).toBeNull();
	});

	test("refuses only when the server says blocked", () => {
		expect(blockMessage({ blocked: true, group: "Artur", blockedWindow: "weekly", blockedPct: 100 })).toBe(
			'usagefleet: "Artur" has used 100% of its weekly budget, so new prompts are blocked.',
		);
		expect(blockMessage({ blocked: false, group: "Artur" })).toBeNull();
	});
});
