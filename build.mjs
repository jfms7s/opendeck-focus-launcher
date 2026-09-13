#!/usr/bin/env node
// Assembles dist/<uuid>.sdPlugin/ from assets/ + a release binary for one target.
// Usage: node build.mjs <target-triple>
// Requires: cargo build --release --target <target-triple> already run for that triple.
import { cpSync, copyFileSync, mkdirSync, rmSync, existsSync } from "node:fs";
import { join } from "node:path";

const UUID = "com.jfms7s.focuslauncher";
const BIN_NAME = "opendeck-focus-launcher";

const target = process.argv[2];
if (!target) {
	console.error("usage: node build.mjs <target-triple>");
	process.exit(1);
}

const binPath = join("target", target, "release", BIN_NAME);
if (!existsSync(binPath)) {
	console.error(`missing release binary: ${binPath} (run: cargo build --release --target ${target})`);
	process.exit(1);
}

const outDir = join("dist", `${UUID}.sdPlugin`);
rmSync(outDir, { recursive: true, force: true });
mkdirSync(outDir, { recursive: true });

cpSync("assets/manifest.json", join(outDir, "manifest.json"));
cpSync("assets/icons", join(outDir, "icons"), { recursive: true });
cpSync("assets/propertyInspector", join(outDir, "propertyInspector"), { recursive: true });
copyFileSync(binPath, join(outDir, `${BIN_NAME}-${target}`));

console.log(`built ${outDir} for ${target}`);
