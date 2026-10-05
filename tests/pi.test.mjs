// Behaviour tests for the property inspector script, run with `node --test`.
// They load the <script> from assets/propertyInspector/index.html into a
// minimal fake DOM whose <select> follows browser semantics (setting a value
// no <option> has selects ""), which is what used to wipe a key's saved app.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";

const html = readFileSync(new URL("../assets/propertyInspector/index.html", import.meta.url), "utf8");
const script = html.match(/<script>([\s\S]*)<\/script>/)[1];

class FakeOption {
	constructor(text, value) {
		this.textContent = text;
		this.value = value;
	}
}

class FakeSelect {
	constructor() {
		this.options = [];
		this.current = "";
		this.listeners = {};
	}
	replaceChildren(...options) {
		this.options = options;
		this.current = options[0] ? options[0].value : "";
	}
	appendChild(option) {
		this.options.push(option);
	}
	get value() {
		return this.current;
	}
	set value(v) {
		this.current = this.options.some((o) => o.value === v) ? v : "";
	}
	addEventListener(event, fn) {
		this.listeners[event] = fn;
	}
}

class FakeInput {
	constructor() {
		this.value = "";
		this.checked = false;
		this.placeholder = "";
		this.textContent = "";
		this.listeners = {};
	}
	addEventListener(event, fn) {
		this.listeners[event] = fn;
	}
}

// Two timer turns: the socket is created in a promise callback and opens on
// its own timer.
const tick = () => new Promise((resolve) => setTimeout(() => setTimeout(resolve, 0), 0));

async function openInspector(settings) {
	const els = { app: new FakeSelect() };
	for (const id of [
		"path",
		"cycle_windows",
		"minimize_when_focused",
		"close_all_windows_on_hold",
		"name_override",
		"icon_override",
		"class_override",
		"exec_override",
		"custom_args",
	]) {
		els[id] = new FakeInput();
	}
	const sent = [];
	let socket;
	const context = {
		document: { getElementById: (id) => els[id] },
		Option: FakeOption,
		WebSocket: class {
			constructor() {
				socket = this;
				this.send = (message) => sent.push(JSON.parse(message));
				setTimeout(() => this.onopen(), 0);
			}
		},
		setTimeout,
	};
	context.window = context;
	vm.runInNewContext(script, context);
	context.connectOpenActionSocket(1, "ctx", "registerPropertyInspector", "{}", JSON.stringify({ payload: { settings } }));
	await tick();
	const receive = (event, payload) => socket.onmessage({ data: JSON.stringify({ event, payload }) });
	const lastSaved = () => sent.filter((m) => m.event === "setSettings").at(-1)?.payload;
	return { els, receive, lastSaved };
}

const APPS = [
	{ id: "org.mozilla.firefox", name: "Firefox", path: "/usr/share/applications/org.mozilla.firefox.desktop", exec: "firefox %u", window_class: "firefox" },
];

test("editing another field keeps a saved app that is not installed", async () => {
	const pi = await openInspector({ app: "org.gone.App", cycle_windows: false, custom_args: "--x" });
	pi.receive("sendToPropertyInspector", { apps: APPS });
	pi.els.minimize_when_focused.checked = false;
	pi.els.minimize_when_focused.listeners.change();
	const saved = pi.lastSaved();
	assert.equal(saved.app, "org.gone.App");
	assert.equal(saved.cycle_windows, false);
	assert.equal(saved.custom_args, "--x");
});

test("editing before the apps list arrives keeps the saved app", async () => {
	const pi = await openInspector({ app: "org.mozilla.firefox" });
	pi.els.name_override.value = "Browser";
	pi.els.name_override.listeners.change();
	assert.equal(pi.lastSaved().app, "org.mozilla.firefox");
	assert.equal(pi.lastSaved().name_override, "Browser");
});

test("a later apps list does not revert a newly picked app", async () => {
	const pi = await openInspector({ app: "org.gone.App" });
	pi.receive("sendToPropertyInspector", { apps: APPS });
	pi.els.app.value = "org.mozilla.firefox";
	pi.els.app.listeners.change();
	pi.receive("sendToPropertyInspector", { apps: APPS });
	assert.equal(pi.els.app.value, "org.mozilla.firefox");
});

test("picking an app resets overrides and shows its window class", async () => {
	const pi = await openInspector({ app: null, class_override: "x", custom_args: "--y" });
	pi.receive("sendToPropertyInspector", { apps: APPS });
	pi.els.app.value = "org.mozilla.firefox";
	pi.els.app.listeners.change();
	const saved = pi.lastSaved();
	assert.equal(saved.app, "org.mozilla.firefox");
	assert.equal(saved.class_override, null);
	assert.equal(saved.custom_args, null);
	assert.equal(pi.els.class_override.placeholder, "firefox");
});

test("defaults match the plugin and blank text is saved as null", async () => {
	const pi = await openInspector({});
	assert.equal(pi.els.cycle_windows.checked, true);
	assert.equal(pi.els.minimize_when_focused.checked, true);
	assert.equal(pi.els.close_all_windows_on_hold.checked, false);
	pi.els.class_override.value = "   ";
	pi.els.class_override.listeners.change();
	const saved = pi.lastSaved();
	assert.equal(saved.class_override, null);
	// Same keys as the payload fixture the Rust contract test deserializes.
	const fixture = JSON.parse(readFileSync(new URL("fixtures/pi-settings.json", import.meta.url), "utf8"));
	assert.deepEqual(Object.keys(saved).sort(), Object.keys(fixture).sort());
});
