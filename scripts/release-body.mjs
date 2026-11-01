#!/usr/bin/env node
// Prints the notes a draft release starts with: the README's manual smoke-test
// checklist (the table between the `smoke-checklist:start` and
// `smoke-checklist:end` markers), one unticked item per row, under a header
// the maintainer fills in before publishing.
//
// Usage (from the repo root):
//   node scripts/release-body.mjs [README.md] > RELEASE_BODY.md
//
// Fails instead of printing a partial body when a marker is missing, doubled
// or out of order, or the block holds no checklist row. CI runs it on every
// PR so a README edit can't break the next tag's Release workflow.
import { readFileSync } from "node:fs";

function fail(message) {
	console.error(`release-body: ${message}`);
	process.exit(1);
}

const START = "<!-- smoke-checklist:start";
const END = "<!-- smoke-checklist:end -->";

const readmePath = process.argv[2] ?? "README.md";
const lines = readFileSync(readmePath, "utf8").split("\n");
const starts = lines.flatMap((l, i) => (l.startsWith(START) ? [i] : []));
const ends = lines.flatMap((l, i) => (l.trim() === END ? [i] : []));
if (starts.length !== 1 || ends.length !== 1) {
	fail(`${readmePath} needs exactly one start and one end smoke-checklist marker (found ${starts.length} and ${ends.length})`);
}
if (ends[0] <= starts[0]) fail(`${readmePath}'s smoke-checklist end marker comes before its start marker`);

// The table: a header row naming the platforms, a separator, then one row per
// check: `| # | Check | <platform result>... |`.
const cells = (row) => row.trim().replace(/^\|/, "").replace(/\|$/, "").split("|").map((c) => c.trim());
const table = lines.slice(starts[0] + 1, ends[0]).filter((l) => l.trim().startsWith("|"));
if (table.length < 3) fail(`${readmePath}'s smoke-checklist block holds no table row`);
const platforms = cells(table[0]).slice(2);
const items = table.slice(2).map((row) => {
	const [number, check, ...results] = cells(row);
	if (!/^\d+$/.test(number) || !check) fail(`not a checklist row: ${row}`);
	const on = platforms.filter((_, i) => results[i] && results[i].toLowerCase() !== "n/a");
	return `- [ ] **${number}.** ${check} _(${on.join(", ")})_`;
});

const version = readFileSync("Cargo.toml", "utf8").match(/^version\s*=\s*"([^"]+)"/m)?.[1];
if (!version) fail('could not find `version = "..."` in Cargo.toml');

process.stdout.write(`<!-- Written by the Release workflow. Before publishing: run the checklist on
a device, tick what passed and mark the rest "fail" or "not run" with why, fill
in the smoke log line, and delete this comment. -->
## Smoke test

**Smoke log:** _(link or summarise the v${version} smoke log here before publishing)_
**Run on:** _(date · OpenDeck version · desktop and session type, or macOS version · Stream Deck model)_

${items.join("\n")}
`);
