// Evaluates an expression in a page through the app's external CDP endpoint
// (MONOCODE_BROWSER_AGENT_DEV's file): node cdp.mjs <agent.json> <url-part> '<expr>' [shot.png]
import { readFileSync, writeFileSync } from "node:fs";
const [file, part, expr, shot] = process.argv.slice(2);
const dev = JSON.parse(readFileSync(file, "utf8"));
const list = await (await fetch(dev.cdp_endpoint + "/json/list")).json();
const page = list.find((t) => t.type === "page" && t.url.includes(part));
if (!page) { console.log("no page", list.map((t) => t.url)); process.exit(1); }
const ws = new WebSocket(page.webSocketDebuggerUrl);
let id = 0;
const waiting = new Map();
const call = (method, params = {}) => new Promise((res) => { const i = ++id; waiting.set(i, res); ws.send(JSON.stringify({ id: i, method, params })); });
ws.onmessage = (m) => { const d = JSON.parse(m.data); if (d.id && waiting.has(d.id)) { waiting.get(d.id)(d); waiting.delete(d.id); } };
await new Promise((r) => (ws.onopen = r));
const r = await call("Runtime.evaluate", { expression: expr, awaitPromise: true, returnByValue: true });
console.log(JSON.stringify(r.result?.result?.value ?? r.result ?? r.error));
if (shot) {
  const s = await call("Page.captureScreenshot", { format: "png" });
  if (s.result?.data) writeFileSync(shot, Buffer.from(s.result.data, "base64"));
  console.log(shot, s.result ? "saved" : JSON.stringify(s.error));
}
ws.close();
process.exit(0);
