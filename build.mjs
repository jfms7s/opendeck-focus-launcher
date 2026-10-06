#!/usr/bin/env node
// Assembles dist/<plugin-uuid>.sdPlugin/ from assets/ plus the release
// binaries for the targets in manifest.json's CodePaths, and optionally zips
// it into <bin-name>.streamDeckPlugin.
//
// Usage:
//   node build.mjs                       package every CodePaths target that has been built
//   node build.mjs --all                 require every CodePaths target (used by the release)
//   node build.mjs <triple>...           package exactly these targets
//   node build.mjs --check-only          only run the consistency checks below
//   options: --zip                       also write <bin-name>.streamDeckPlugin
//            --expect-version <v|vX.Y.Z> fail unless Cargo.toml and manifest.json are at this version
//
// Build the binaries first: cargo build --release --locked --target <triple>
// (a plain `cargo build --release` counts for the host triple).
//
// Names are read from Cargo.toml ([package] name) and manifest.json (the
// plugin UUID is the first action's UUID minus its last segment), so this
// file carries no per-plugin constants.
import { cpSync, copyFileSync, existsSync, mkdirSync, readFileSync, rmSync, statSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { join } from "node:path";

function fail(message) {
	console.error(`build.mjs: ${message}`);
	process.exit(1);
}

// --- arguments -------------------------------------------------------------
const args = process.argv.slice(2);
let requireAll = false;
let checkOnly = false;
let zip = false;
let expectVersion = null;
const requested = [];
for (let i = 0; i < args.length; i++) {
	const arg = args[i];
	if (arg === "--all") requireAll = true;
	else if (arg === "--check-only") checkOnly = true;
	else if (arg === "--zip") zip = true;
	else if (arg === "--expect-version") {
		expectVersion = args[++i];
		if (!expectVersion) fail("--expect-version needs a value");
		expectVersion = expectVersion.replace(/^v/, "");
	} else if (arg.startsWith("-")) fail(`unknown option ${arg}`);
	else requested.push(arg);
}
if (requireAll && requested.length) fail("pass either --all or explicit targets, not both");

// --- metadata and consistency checks --------------------------------------
const cargoToml = readFileSync("Cargo.toml", "utf8");
const packageSection = cargoToml.split(/^\[/m).find((s) => s.startsWith("package]")) ?? "";
const field = (name) => packageSection.match(new RegExp(`^${name}\\s*=\\s*"([^"]+)"`, "m"))?.[1];
const binName = field("name") ?? fail('no `name = "..."` in Cargo.toml [package]');
const cargoVersion = field("version") ?? fail('no `version = "..."` in Cargo.toml [package]');

const manifest = JSON.parse(readFileSync("assets/manifest.json", "utf8"));
if (cargoVersion !== manifest.Version) {
	fail(`version mismatch: Cargo.toml is ${cargoVersion} but assets/manifest.json is ${manifest.Version} - bump them together`);
}
if (expectVersion !== null && expectVersion !== cargoVersion) {
	fail(`version mismatch: expected ${expectVersion} (from the tag) but Cargo.toml and manifest.json are ${cargoVersion}`);
}

const actionUuid = manifest.Actions?.[0]?.UUID ?? fail("manifest.json has no Actions[0].UUID");
const pluginUuid = actionUuid.split(".").slice(0, -1).join(".");
if (!pluginUuid) fail(`cannot derive the plugin UUID from action UUID ${actionUuid}`);
const rustUuid = readFileSync("src/action.rs", "utf8").match(/const UUID: &'static str = "([^"]+)"/)?.[1];
if (rustUuid !== actionUuid) fail(`action UUID mismatch: manifest.json has ${actionUuid}, src/action.rs has ${rustUuid}`);

const codePaths = manifest.CodePaths ?? fail("manifest.json has no CodePaths");
for (const [triple, file] of Object.entries(codePaths)) {
	if (file !== `${binName}-${triple}`) fail(`CodePaths["${triple}"] is ${file}, expected ${binName}-${triple}`);
}
for (const key of ["CodePathLin", "CodePathMac"]) {
	if (manifest[key] && !Object.values(codePaths).includes(manifest[key])) {
		fail(`${key} ${manifest[key]} is not one of the CodePaths binaries`);
	}
}

if (checkOnly) {
	console.log(`build.mjs: ${binName} ${cargoVersion} metadata is consistent`);
	process.exit(0);
}

// --- pick the binaries -------------------------------------------------------
function hostTriple() {
	try {
		return execFileSync("rustc", ["-vV"], { encoding: "utf8" }).match(/^host: (.+)$/m)?.[1];
	} catch {
		return undefined;
	}
}
const host = hostTriple();

// The newest build for a triple: target/<triple>/release, or target/release
// for the host triple.
function binaryFor(triple) {
	const candidates = [join("target", triple, "release", binName)];
	if (triple === host) candidates.push(join("target", "release", binName));
	return candidates
		.filter((p) => existsSync(p))
		.sort((a, b) => statSync(b).mtimeMs - statSync(a).mtimeMs)[0];
}

let targets;
if (requested.length) {
	for (const t of requested) if (!(t in codePaths)) fail(`${t} is not in manifest.json CodePaths`);
	targets = requested;
} else {
	targets = Object.keys(codePaths);
}
const found = targets.map((t) => [t, binaryFor(t)]);
const missing = found.filter(([, bin]) => !bin).map(([t]) => t);
if ((requireAll || requested.length) && missing.length) {
	fail(`missing release binaries for ${missing.join(", ")} (run: cargo build --release --locked --target <triple>)`);
}
const built = found.filter(([, bin]) => bin);
if (!built.length) fail(`no release binary found for any of ${targets.join(", ")} (run: cargo build --release --locked)`);
for (const t of missing) console.warn(`build.mjs: skipping ${t} (not built)`);

// --- assemble ---------------------------------------------------------------
const bundle = `${pluginUuid}.sdPlugin`;
const outDir = join("dist", bundle);
rmSync(outDir, { recursive: true, force: true });
mkdirSync(outDir, { recursive: true });
cpSync("assets/manifest.json", join(outDir, "manifest.json"));
// Not every plugin has layouts or a property inspector - copy what exists.
// Icon sources (assets/icon-src/) are deliberately not shipped.
for (const dir of ["icons", "layouts", "propertyInspector"]) {
	if (existsSync(join("assets", dir))) cpSync(join("assets", dir), join(outDir, dir), { recursive: true });
}
for (const [triple, bin] of built) {
	copyFileSync(bin, join(outDir, codePaths[triple]));
	console.log(`build.mjs: ${triple} <- ${bin}`);
}
console.log(`built ${outDir} (${built.map(([t]) => t).join(", ")})`);

if (zip) {
	const archive = `${binName}.streamDeckPlugin`;
	rmSync(archive, { force: true });
	execFileSync("zip", ["-r", "-X", join("..", archive), bundle], { cwd: "dist", stdio: "inherit" });
	console.log(`wrote ${archive}`);
}
