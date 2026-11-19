const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (id) => document.getElementById(id);
const query = $("query");
const outputDir = $("outputDir");
const format = $("format");
const bitrate = $("bitrate");
const naming = $("naming");
const downloadBtn = $("downloadBtn");
const cancelBtn = $("cancelBtn");
const browseBtn = $("browseBtn");
const statusText = $("statusText");
const logBox = $("logBox");
const badge = $("engineBadge");

const MAX_LOG_LINES = 2000;
let engineReady = false;
let running = false;

function setStatus(text, kind = "") {
  statusText.textContent = text;
  statusText.className = "status " + kind;
}

function appendLog(line) {
  const atBottom = logBox.scrollHeight - logBox.scrollTop - logBox.clientHeight < 24;
  const lines = (logBox.textContent + "\n" + line).split("\n");
  logBox.textContent = lines.slice(-MAX_LOG_LINES).join("\n");
  if (atBottom) logBox.scrollTop = logBox.scrollHeight;
}

function settings() {
  return {
    output_dir: outputDir.value.trim(),
    format: format.value,
    bitrate: bitrate.value,
    naming: naming.value,
  };
}

function syncControls() {
  downloadBtn.disabled = !engineReady || running;
  cancelBtn.disabled = !running;
  // Lossless formats ignore bitrate.
  bitrate.disabled = format.value === "flac" || format.value === "wav";
}

function saveSettings() {
  invoke("save_settings", { settings: settings() }).catch((e) => appendLog(String(e)));
}

function plural(n, word) {
  return `${n} ${word}${n === 1 ? "" : "s"}`;
}

async function startDownload() {
  if (!engineReady || running) return;
  const text = query.value.trim();
  if (!text) {
    setStatus("Paste a Spotify link or type a song name.", "error");
    query.focus();
    return;
  }
  running = true;
  syncControls();
  setStatus("Downloading...", "running");
  appendLog(`\n> ${text}`);
  try {
    await invoke("start_download", { query: text, settings: settings() });
  } catch (e) {
    running = false;
    syncControls();
    setStatus(String(e), "error");
  }
}

listen("log", (event) => appendLog(event.payload));

listen("done", (event) => {
  const s = event.payload;
  running = false;
  syncControls();
  if (s.cancelled) {
    setStatus("Cancelled", "error");
    appendLog("> Cancelled.");
    return;
  }
  const parts = [];
  if (s.saved) parts.push(`Saved ${plural(s.saved, "song")}`);
  if (s.skipped) parts.push(`${s.skipped} already in the folder`);
  if (s.failed) parts.push(`${s.failed} couldn't be found`);
  if (s.saved || s.skipped) {
    setStatus(parts.join(", "), s.failed ? "error" : "ok");
  } else {
    setStatus(parts.length ? parts.join(", ") : "Download failed. See the log for details.", "error");
  }
});

downloadBtn.addEventListener("click", startDownload);
query.addEventListener("keydown", (e) => { if (e.key === "Enter") startDownload(); });
cancelBtn.addEventListener("click", () => {
  setStatus("Stopping...", "running");
  invoke("cancel_download");
});
$("clearBtn").addEventListener("click", () => { logBox.textContent = ""; });
$("openBtn").addEventListener("click", () => {
  invoke("open_folder", { path: outputDir.value }).catch((e) => setStatus(String(e), "error"));
});
browseBtn.addEventListener("click", async () => {
  browseBtn.disabled = true;
  try {
    const picked = await invoke("browse_folder", { current: outputDir.value });
    if (picked) {
      outputDir.value = picked;
      saveSettings();
    }
  } finally {
    browseBtn.disabled = false;
  }
});
for (const el of [outputDir, format, bitrate, naming]) {
  el.addEventListener("change", () => { syncControls(); saveSettings(); });
}

(async () => {
  const s = await invoke("get_settings");
  outputDir.value = s.output_dir;
  format.value = s.format;
  bitrate.value = s.bitrate;
  naming.value = s.naming;
  syncControls();
  query.focus();

  const engine = await invoke("engine_status");
  engineReady = engine.ready;
  if (engine.ready) {
    badge.textContent = `spotDL ${engine.version} ready`;
    badge.className = "badge";
  } else {
    badge.textContent = "spotDL unavailable";
    badge.className = "badge error";
    setStatus(engine.error, "error");
    appendLog(engine.error);
  }
  syncControls();
})();
