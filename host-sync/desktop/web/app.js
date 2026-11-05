const { invoke } = window.__TAURI__.core;
const element = (id) => document.getElementById(id);
let current;
let pending = false;
let actionError = "";
let editorLoaded = false;

function render(status) {
  current = status;
  element("sync-status").hidden = !status.configured;
  element("sync-actions").hidden = !status.configured;
  const running = status.phase === "running";
  const stopping = status.phase === "stopping";
  const errors = [actionError, status.calendar_error, status.privacy_error].filter(Boolean);
  let phase = "Sync stopped";
  let activity = "Start to sync your calendar in the background.";
  if (!status.configured) activity = "Set your frame’s USB port, then save configuration to get started.";
  let dot = running ? "running" : "";
  if (stopping) {
    phase = "Stopping sync…";
    activity = "Finishing the current operation. No new calendar sync will start.";
  } else if (running) {
    phase = "Sync running";
    activity = "Next calendar refresh runs automatically.";
    if (status.syncing) activity = "Fetching and sending your calendar.";
    else if (status.calendar_error) activity = "Retrying next cycle. Previous frame data retained.";
  }
  if (errors.length) dot = "error";
  element("phase").textContent = phase;
  element("dot").className = `dot ${dot}`;
  element("activity").textContent = activity;
  element("last").textContent = status.last_synced ? new Date(status.last_synced).toLocaleString([], { dateStyle: "short", timeStyle: "short" }) : "Not yet";
  element("events").textContent = status.events ?? "—";
  let privacy = "Stopped";
  if (running) {
    privacy = "Monitoring host lock state";
    if (!status.privacy_supported) privacy = "Unavailable on raw transport";
    else if (status.privacy_error) privacy = "Failed · check connection";
  }
  element("privacy").textContent = privacy;
  element("config").textContent = status.config ?? "~/.config/home-dash/host-sync.json";
  const editingDisabled = pending || running || stopping;
  for (const id of ["choose", "edit", "save"]) element(id).disabled = editingDisabled;
  element("config-text").readOnly = editingDisabled;
  let toggle = running ? "Stop sync" : "Start sync";
  if (stopping) toggle = "Stopping…";
  element("toggle").textContent = toggle;
  element("toggle").disabled = pending || stopping || (!running && !status.configured);
  element("sync").disabled = pending || !running || status.syncing;
  element("errors").textContent = errors.join("\n\n");
  element("errors").hidden = errors.length === 0;
}

async function refresh() {
  try {
    render(await invoke("status"));
  } catch (error) {
    element("errors").hidden = false;
    element("errors").textContent = String(error);
  }
}

async function act(action) {
  pending = true;
  actionError = "";
  render(current);
  try {
    await action();
  } catch (error) {
    actionError = String(error);
  } finally {
    pending = false;
    await refresh();
  }
}

async function edit() {
  try {
    element("config-text").value = await invoke("load_config");
    element("editor").hidden = false;
    editorLoaded = true;
  } catch (error) {
    actionError = String(error);
    render(current);
  }
}

element("edit").addEventListener("click", edit);
element("cancel").addEventListener("click", () => { element("editor").hidden = true; });
element("save").addEventListener("click", () => act(async () => {
  await invoke("save_config", { contents: element("config-text").value });
  element("editor").hidden = true;
}));
element("choose").addEventListener("click", () => act(async () => {
  const outcome = await invoke("choose_config");
  if (outcome === "imported") await edit();
}));
element("toggle").addEventListener("click", () => act(() => invoke(current.phase === "running" ? "stop_sync" : "start_sync")));
element("sync").addEventListener("click", () => act(() => invoke("sync_now")));
async function poll() {
  await refresh();
  if (current && !current.configured && !editorLoaded) await edit();
  setTimeout(poll, 1000);
}
poll();
