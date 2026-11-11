// RustOp Viewer: shows the remote screen, sends mouse and keyboard input
// back in small batches, and carries clipboard text both ways.
const $ = (id) => document.getElementById(id);
const img = $("screen"), status = $("status"), panel = $("clip"), area = $("cliptext");
const MAX_CLIP = 1 << 20;
let width = 0, height = 0, queue = [], frames = 0, quiet;
let clipboard = false; // the host can share its clipboard
let hold = false;      // a paste is setting the remote clipboard: input waits for it
let sending = false;   // one input batch at a time, so keys arrive in order
let known = null;      // the clipboard text both sides last agreed on
const held = new Set();

function say(text) {
  status.textContent = text;
  status.style.display = "";
  clearTimeout(quiet);
  quiet = setTimeout(() => { status.style.display = "none"; }, 5000);
}

fetch("/info").then((r) => r.json()).then((info) => {
  width = info.width; height = info.height;
  say(`${width}×${height}` + (info.error ? ` · ${info.error}` : ""));
  clipboard = info.clipboard;
  $("bar").hidden = !clipboard;
  if (clipboard) remoteText().then((text) => { known = text; }, () => {});
});
img.src = "/stream";
img.addEventListener("load", () => { frames++; });

function send(event) {
  if (event.t === "move" && queue.length && queue[queue.length - 1].t === "move") queue[queue.length - 1] = event;
  else queue.push(event);
}
setInterval(() => {
  if (!queue.length || hold || sending) return;
  const batch = queue; queue = []; sending = true;
  fetch("/input", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(batch) })
    .catch(() => say("connection lost"))
    .finally(() => { sending = false; });
}, 30);

// Where on the remote screen a point in the window is (the image is
// scaled to fit, keeping its shape).
function position(ev) {
  const box = img.getBoundingClientRect();
  if (!width || !height) return null;
  const scale = Math.min(box.width / width, box.height / height);
  const left = box.left + (box.width - width * scale) / 2;
  const top = box.top + (box.height - height * scale) / 2;
  return { x: Math.round((ev.clientX - left) / scale), y: Math.round((ev.clientY - top) / scale) };
}
const button = (ev) => (ev.button === 0 ? 1 : ev.button === 1 ? 2 : ev.button === 2 ? 3 : 0);

img.addEventListener("mousemove", (ev) => { const p = position(ev); if (p) send({ t: "move", x: p.x, y: p.y }); });
img.addEventListener("mousedown", (ev) => { ev.preventDefault(); const p = position(ev); if (p) send({ t: "move", x: p.x, y: p.y }); send({ t: "down", b: button(ev) }); });
img.addEventListener("mouseup", (ev) => { ev.preventDefault(); send({ t: "up", b: button(ev) }); });
img.addEventListener("contextmenu", (ev) => ev.preventDefault());
let wheel = 0;
img.addEventListener("wheel", (ev) => {
  ev.preventDefault();
  wheel += ev.deltaY;
  const steps = Math.trunc(wheel / 60);
  if (steps) { send({ t: "wheel", dy: steps }); wheel -= steps * 60; }
}, { passive: false });

const inPanel = (ev) => ev.target instanceof Element && ev.target.closest("#clip") !== null;
const shortcut = (ev, keys) => (ev.ctrlKey || ev.metaKey) && !ev.altKey && keys.includes(ev.key.toLowerCase());

window.addEventListener("keydown", (ev) => {
  if (inPanel(ev)) return;
  const paste = clipboard && !ev.repeat && shortcut(ev, ["v"]);
  if (!paste) ev.preventDefault(); // a paste shortcut goes on, so the browser fires "paste"
  held.add(ev.key);
  send({ t: "key", k: ev.key, d: true, s: ev.shiftKey });
  if (paste) startPaste();
  else if (clipboard && !ev.repeat && shortcut(ev, ["c", "x"])) afterCopy();
});
window.addEventListener("keyup", (ev) => {
  if (inPanel(ev)) return;
  ev.preventDefault();
  held.delete(ev.key);
  send({ t: "key", k: ev.key, d: false, s: ev.shiftKey });
});
// Leaving the window must not leave keys pressed on the remote side.
window.addEventListener("blur", () => {
  for (const key of held) send({ t: "key", k: key, d: false, s: false });
  held.clear();
});

// The clipboard. Text travels only in same-origin JSON requests.
async function post(path, body) {
  const r = await fetch(path, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
  if (!r.ok) throw new Error((await r.text()).trim() || `the host answered ${r.status}`);
  return r;
}
async function remoteText() { return (await (await post("/clipboard/get", {})).json()).text; }

// Paste: the remote gets the text first, then the keys that paste it.
// Ctrl+V (⌘V) makes the browser hand its clipboard to the page in a
// "paste" event, without a prompt; if none comes, the page asks for it.
let pasteWait;
function startPaste() {
  hold = true;
  pasteWait = setTimeout(() => {
    readClipboard().then((text) => finishPaste(text, false), () => {
      hold = false;
      say("the browser did not let this page read its clipboard: use Paste here");
    });
  }, 300);
}
document.addEventListener("paste", (ev) => {
  if (inPanel(ev) || !clipboard) return;
  ev.preventDefault();
  const keysQueued = hold;
  clearTimeout(pasteWait);
  hold = true;
  finishPaste(ev.clipboardData ? ev.clipboardData.getData("text/plain") : "", !keysQueued);
});
function readClipboard() {
  return navigator.clipboard && navigator.clipboard.readText ? navigator.clipboard.readText() : Promise.reject(new Error("no clipboard access"));
}
async function finishPaste(text, pressKeys) {
  try {
    if (!text) { say("this computer's clipboard holds no text"); return; }
    if (new TextEncoder().encode(text).length > MAX_CLIP) throw new Error("the clipboard holds more than 1 MB of text; RustOp Viewer carries up to 1 MB");
    await post("/clipboard/set", { text });
    known = text;
    if (pressKeys) for (const [k, d] of [["Control", true], ["v", true], ["v", false], ["Control", false]]) send({ t: "key", k, d, s: false });
  } catch (e) {
    say(`paste: ${e.message}`);
  } finally {
    hold = false;
  }
}
$("paste").addEventListener("click", (ev) => {
  ev.currentTarget.blur();
  hold = true;
  readClipboard().then((text) => finishPaste(text, true), () => { hold = false; openPanel("paste"); });
});

// Copy: after a copy shortcut, or the button, the remote's clipboard
// comes here. After a shortcut only new text is written, so Ctrl+C in a
// remote terminal does not replace what this computer copied.
async function fromRemote(always) {
  let text;
  try { text = await remoteText(); } catch (e) { say(`copy: ${e.message}`); return false; }
  if (!text) { if (always) say("the remote clipboard holds no text"); return false; }
  if (!always && text === known) return false;
  known = text;
  try {
    await navigator.clipboard.writeText(text);
    say("copied from the remote");
  } catch {
    openPanel("copy", text);
  }
  return true;
}
function afterCopy() {
  setTimeout(async () => { if (!(await fromRemote(false))) setTimeout(() => fromRemote(false), 800); }, 250);
}
$("copy").addEventListener("click", (ev) => { ev.currentTarget.blur(); fromRemote(true); });

// When the browser refuses clipboard access, a box does it by hand.
function openPanel(mode, text) {
  $("clipnote").textContent = mode === "paste"
    ? "The browser does not let this page read its clipboard. Paste into this box (Ctrl+V or ⌘V), then Send to remote."
    : "The browser does not let this page write its clipboard. The remote's text is selected: press Ctrl+C (or ⌘C) to copy it.";
  $("clipsend").hidden = mode !== "paste";
  panel.hidden = false;
  area.value = text || "";
  area.focus();
  area.select();
}
function closePanel() { panel.hidden = true; area.value = ""; }
$("clipclose").addEventListener("click", closePanel);
$("clipsend").addEventListener("click", () => { const text = area.value; closePanel(); hold = true; finishPaste(text, true); });
