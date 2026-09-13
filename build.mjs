#!/usr/bin/env node
// Assembles dist/<uuid>.sdPlugin/ from assets/ + one release binary per given
// target, all in a single bundle (a "universal" .streamDeckPlugin - matching
// the reference me.amankhanna.oadesktopentry plugin's layout, which ships
// every supported architecture's binary alongside one manifest.json).
//
// Usage: node build.mjs <target-triple> [target-triple ...]
// Requires: `cargo build --release --target <target-triple>` already run for
// every triple passed in.
import { cpSync, copyFileSync, mkdirSync, rmSync, existsSync } from "node:fs";
import { join } from "node:path";

const UUID = "com.jfms7s.focuslauncher";
const BIN_NAME = "opendeck-focus-launcher";

const targets = process.argv.slice(2);
if (targets.length === 0) {
	console.error("usage: node build.mjs <target-triple> [target-triple ...]");
	process.exit(1);
}

const binPaths = targets.map((target) => join("target", target, "release", BIN_NAME));
for (const [target, binPath] of targets.map((t, i) => [t, binPaths[i]])) {
	if (!existsSync(binPath)) {
		console.error(`missing release binary: ${binPath} (run: cargo build --release --target ${target})`);
		process.exit(1);
	}
}

const outDir = join("dist", `${UUID}.sdPlugin`);
rmSync(outDir, { recursive: true, force: true });
mkdirSync(outDir, { recursive: true });

cpSync("assets/manifest.json", join(outDir, "manifest.json"));
cpSync("assets/icons", join(outDir, "icons"), { recursive: true });
cpSync("assets/propertyInspector", join(outDir, "propertyInspector"), { recursive: true });

for (const [target, binPath] of targets.map((t, i) => [t, binPaths[i]])) {
	copyFileSync(binPath, join(outDir, `${BIN_NAME}-${target}`));
}

console.log(`built ${outDir} for ${targets.join(", ")}`);
