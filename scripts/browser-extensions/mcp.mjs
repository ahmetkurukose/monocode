// Calls one monocode_browser tool as an agent would, through the app's
// `browser-mcp` subcommand (the env comes from MONOCODE_BROWSER_AGENT_DEV's
// file): node mcp.mjs <agent.json> <tool> '<json arguments>'
import { spawn } from "node:child_process";
import { readFileSync } from "node:fs";
const [file, tool, args = "{}"] = process.argv.slice(2);
const dev = JSON.parse(readFileSync(file, "utf8"));
const env = { ...process.env, ...dev.bridge_env };
const child = spawn(env.MONOCODE_BROWSER_EXECUTABLE, ["browser-mcp"], { env, stdio: ["pipe", "pipe", "inherit"] });
let buf = "";
const send = (m) => child.stdin.write(JSON.stringify(m) + "\n");
child.stdout.on("data", (d) => {
  buf += d;
  for (;;) {
    const at = buf.indexOf("\n");
    if (at < 0) break;
    const line = buf.slice(0, at);
    buf = buf.slice(at + 1);
    const m = JSON.parse(line);
    if (m.id === 1) send({ jsonrpc: "2.0", id: 2, method: "tools/call", params: { name: tool, arguments: JSON.parse(args) } });
    if (m.id === 2) {
      for (const c of m.result?.content || []) console.log(c.type === "text" ? c.text : `[${c.type}]`);
      if (m.error) console.log(JSON.stringify(m.error));
      child.kill();
      process.exit(0);
    }
  }
});
send({ jsonrpc: "2.0", id: 1, method: "initialize", params: { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "ext-test", version: "1" } } });
setTimeout(() => { console.log("timeout"); child.kill(); process.exit(1); }, 60000);
