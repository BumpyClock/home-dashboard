const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");

const elements = new Map();
const get = (id) => {
  if (!elements.has(id)) elements.set(id, { addEventListener(name, fn) { this[name] = fn; } });
  return elements.get(id);
};
const defaultPath = "/Users/test/.config/home-dash/host-sync.json";
let contents = '{"connection":{"kind":"console","port":""},"calendar":{"sources":[]}}';
let status = { phase: "stopped", config: defaultPath, configured: false, syncing: false, last_synced: null, events: null, calendar_error: null, privacy_error: null, privacy_supported: false };
let timer;
let failure;
let cancelDialog = false;
const calls = [];
const context = vm.createContext({
  document: { getElementById: get },
  window: { __TAURI__: { core: { invoke: async (command, args) => {
    calls.push(command);
    if (command === "status") return { ...status };
    if (failure) throw failure;
    if (command === "load_config") return contents;
    if (command === "save_config") { contents = args.contents; status.configured = true; }
    if (command === "choose_config") {
      if (cancelDialog) return "cancelled";
      status.configured = true;
      return "imported";
    }
    if (command === "start_sync") { status.phase = "running"; status.privacy_supported = true; }
    if (command === "stop_sync") status.phase = "stopping";
    if (command === "sync_now") status.syncing = true;
  } } } },
  setTimeout(fn) { timer = fn; },
});
const settle = () => new Promise((resolve) => setImmediate(resolve));

(async () => {
  vm.runInContext(fs.readFileSync(path.join(__dirname, "../web/app.js"), "utf8"), context);
  await settle();
  assert.equal(get("phase").textContent, "Sync stopped");
  assert.equal(get("toggle").disabled, true);
  assert.equal(get("choose").disabled, false);
  assert.equal(get("editor").hidden, false, "First launch opens setup, not a file picker");
  assert.equal(get("sync-status").hidden, true);
  assert.equal(get("config-text").value, contents);
  assert.equal(calls.includes("choose_config"), false);
  const edited = '{"connection":{"kind":"console","port":"test"},"calendar":{"sources":[]}}';
  get("config-text").value = edited;
  await timer();
  assert.equal(get("config-text").value, edited, "Polling must preserve unsaved edits");
  failure = "serial port must not be empty";
  await get("save").click();
  assert.equal(get("editor").hidden, false);
  assert.match(get("errors").textContent, /serial port/);
  failure = null;
  await get("save").click();
  assert.equal(contents, edited);
  assert.equal(get("editor").hidden, true);
  assert.equal(get("sync-status").hidden, false);
  cancelDialog = true;
  await get("choose").click();
  assert.equal(get("config").textContent, defaultPath);
  assert.equal(get("editor").hidden, true, "Canceled import must not open the editor");
  await get("edit").click();
  const unsaved = edited.replace("test", "unsaved-port");
  get("config-text").value = unsaved;
  await get("choose").click();
  assert.equal(get("config-text").value, unsaved, "Canceled import must preserve unsaved edits");
  assert.equal(get("editor").hidden, false);
  failure = "Import failed";
  await get("choose").click();
  assert.equal(get("config-text").value, unsaved, "Failed import must preserve unsaved edits");
  assert.match(get("errors").textContent, /Import failed/);
  failure = null;
  cancelDialog = false;
  await get("choose").click();
  assert.equal(get("config").textContent, defaultPath, "Import copies into the default file");
  assert.equal(get("config-text").value, contents, "Successful import reloads saved configuration");
  assert.equal(get("editor").hidden, false);
  await get("cancel").click();
  assert.equal(get("toggle").disabled, false);
  await get("toggle").click();
  assert.equal(calls.at(-2), "start_sync");
  assert.equal(get("toggle").textContent, "Stop sync");
  assert.equal(get("choose").disabled, true);
  assert.equal(get("edit").disabled, true);
  assert.equal(get("config-text").readOnly, true);
  await get("sync").click();
  assert.equal(get("sync").disabled, true);
  status.syncing = false;
  status.last_synced = "2026-11-02T18:00:00Z";
  status.events = 0;
  status.privacy_error = "USB disconnected";
  status.calendar_error = "Provider failed";
  await timer();
  assert.equal(get("events").textContent, 0);
  assert.match(get("privacy").textContent, /Failed/);
  assert.match(get("activity").textContent, /Previous frame data retained/);
  assert.match(get("errors").textContent, /USB disconnected/);
  assert.match(get("errors").textContent, /Provider failed/);
  status.calendar_error = null;
  await timer();
  assert.equal(get("errors").hidden, false, "Calendar recovery must not hide heartbeat failures");
  status.privacy_error = null;
  status.privacy_supported = false;
  await timer();
  assert.match(get("privacy").textContent, /raw transport/);
  assert.equal(get("errors").hidden, true);
  await get("toggle").click();
  assert.equal(get("phase").textContent, "Stopping sync…");
  assert.equal(get("toggle").disabled, true);
  assert.equal(get("choose").disabled, true);
  status.phase = "stopped";
  await timer();
  failure = "Invalid configuration";
  await get("toggle").click();
  assert.match(get("errors").textContent, /Invalid configuration/);
  assert.equal(get("toggle").disabled, false);
  console.log("PASS default config setup/edit/import, controls, independent errors and stopping states");
})().catch((error) => { console.error(error); process.exitCode = 1; });
