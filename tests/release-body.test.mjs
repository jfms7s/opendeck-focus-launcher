// Tests for scripts/release-body.mjs, which writes each draft release's notes
// from the README's smoke-test table.
import { test } from "node:test";
import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const root = new URL("..", import.meta.url).pathname;
const script = join(root, "scripts/release-body.mjs");

function run(readme) {
	return spawnSync(process.execPath, [script, ...(readme ? [readme] : [])], { cwd: root, encoding: "utf8" });
}

function withReadme(text) {
	const path = join(mkdtempSync(join(tmpdir(), "release-body-")), "README.md");
	writeFileSync(path, text);
	return path;
}

const TABLE = `| # | Check | KDE | macOS |
|---|---|---|---|
| 1 | Press launches. | not yet run | not yet run |
| 2 | Mac only. | n/a | OK |
`;

test("the real README gives one unticked item per smoke-test row", () => {
	const body = execFileSync(process.execPath, [script], { cwd: root, encoding: "utf8" });
	const rows = readFileSync(join(root, "README.md"), "utf8").match(/^\| \d+ \|/gm).length;
	assert.equal(body.match(/^- \[ \] \*\*\d+\.\*\* /gm).length, rows);
	assert.match(body, /^## Smoke test$/m);
	const version = readFileSync(join(root, "Cargo.toml"), "utf8").match(/^version\s*=\s*"([^"]+)"/m)[1];
	assert.ok(body.includes(`v${version} smoke log`));
	assert.doesNotMatch(body, /\[x\]/i);
});

test("each item names the platforms it applies to", () => {
	const out = run(withReadme(`<!-- smoke-checklist:start -->\n${TABLE}<!-- smoke-checklist:end -->\n`));
	assert.equal(out.status, 0, out.stderr);
	assert.match(out.stdout, /^- \[ \] \*\*1\.\*\* Press launches\. _\(KDE, macOS\)_$/m);
	assert.match(out.stdout, /^- \[ \] \*\*2\.\*\* Mac only\. _\(macOS\)_$/m);
});

for (const [name, text] of [
	["a missing end marker", `<!-- smoke-checklist:start -->\n${TABLE}`],
	["a doubled start marker", `<!-- smoke-checklist:start -->\n<!-- smoke-checklist:start -->\n${TABLE}<!-- smoke-checklist:end -->\n`],
	["markers out of order", `<!-- smoke-checklist:end -->\n${TABLE}<!-- smoke-checklist:start -->\n`],
	["an empty block", "<!-- smoke-checklist:start -->\n<!-- smoke-checklist:end -->\n"],
	["a malformed row", "<!-- smoke-checklist:start -->\n| # | Check | KDE |\n|---|---|---|\n| x | | |\n<!-- smoke-checklist:end -->\n"],
]) {
	test(`fails on ${name}`, () => {
		const out = run(withReadme(text));
		assert.equal(out.status, 1);
		assert.equal(out.stdout, "");
		assert.match(out.stderr, /^release-body: /);
	});
}
