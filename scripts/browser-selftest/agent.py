#!/usr/bin/env python3
"""Drives the development build's browser tools the way a provider does:
starts the app in a world of its own, then talks MCP to `browser-mcp`."""
import json, os, subprocess, sys, time, shutil
ROOT="/tmp/mc-selftest"; APP="/Users/ahmet/Dev/monocode/crates/monocode-gpui/target/native-dev/monocode-gpui"
for d in ("data","downloads"): shutil.rmtree(f"{ROOT}/{d}", ignore_errors=True); os.makedirs(f"{ROOT}/{d}")
envfile=f"{ROOT}/agent-env.json"
if os.path.exists(envfile): os.remove(envfile)
env=dict(os.environ, MONOCODE_DRAW_HIDDEN="1", MONOCODE_NATIVE_DATA_DIR=f"{ROOT}/data",
         MONOCODE_BROWSER_DOWNLOADS=f"{ROOT}/downloads", MONOCODE_BROWSER_AGENT_TEST=envfile)
app=subprocess.Popen([APP,"--dev-data","--cwd",f"{ROOT}/proj"],env=env,stdout=open(f"{ROOT}/app.log","w"),stderr=subprocess.STDOUT)
try:
    for _ in range(40):
        if os.path.exists(envfile): break
        time.sleep(0.5)
    report=json.load(open(envfile))
    if "error" in report: sys.exit("app: "+report["error"])
    mcp=subprocess.Popen([APP,"browser-mcp"],env=dict(os.environ,**report["environment"]),stdin=subprocess.PIPE,stdout=subprocess.PIPE,text=True)
    n=[0]
    def rpc(method,params=None):
        n[0]+=1
        mcp.stdin.write(json.dumps({"jsonrpc":"2.0","id":n[0],"method":method,"params":params or {}})+"\n"); mcp.stdin.flush()
        return json.loads(mcp.stdout.readline())
    def tool(name,**args):
        r=rpc("tools/call",{"name":name,"arguments":args})["result"]
        parts=r["content"]; text=" ".join(p.get("text","<image %d bytes b64>"%len(p.get("data",""))) for p in parts)
        print(("ERR " if r["isError"] else "ok  ")+name+" "+json.dumps(args)[:70]+"\n      -> "+text[:int(os.environ.get("W","420"))].replace("\n","\n         "))
        return r
    print("init:",rpc("initialize",{"protocolVersion":"2025-03-26"})["result"]["serverInfo"])
    print("tools:",len(rpc("tools/list")["result"]["tools"]))
    exec(open(sys.argv[1]).read() if len(sys.argv)>1 else "tool('browser_pages')")
finally:
    time.sleep(0.5); app.kill()
