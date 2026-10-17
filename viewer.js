// RustOp Viewer: shows the remote screen and sends mouse and keyboard
// input back in small batches.
const img = document.getElementById("screen");
const status = document.getElementById("status");
let width = 0, height = 0, queue = [], frames = 0;
const held = new Set();

fetch("/info").then((r) => r.json()).then((info) => {
  width = info.width; height = info.height;
  status.textContent = `${width}×${height}` + (info.error ? ` · ${info.error}` : "");
});
img.src = "/stream";
img.addEventListener("load", () => { frames++; });
setTimeout(() => { status.style.display = "none"; }, 4000);

function send(event) {
  if (event.t === "move" && queue.length && queue[queue.length - 1].t === "move") queue[queue.length - 1] = event;
  else queue.push(event);
}
setInterval(() => {
  if (!queue.length) return;
  const batch = queue; queue = [];
  fetch("/input", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(batch) })
    .catch(() => { status.style.display = ""; status.textContent = "connection lost"; });
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

window.addEventListener("keydown", (ev) => {
  ev.preventDefault();
  held.add(ev.key);
  send({ t: "key", k: ev.key, d: true, s: ev.shiftKey });
});
window.addEventListener("keyup", (ev) => {
  ev.preventDefault();
  held.delete(ev.key);
  send({ t: "key", k: ev.key, d: false, s: ev.shiftKey });
});
// Leaving the window must not leave keys pressed on the remote side.
window.addEventListener("blur", () => {
  for (const key of held) send({ t: "key", k: key, d: false, s: false });
  held.clear();
});
