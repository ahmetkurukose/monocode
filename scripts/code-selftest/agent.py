#!/usr/bin/env python3
"""Steps 11 and 12: the code tools driven the way a provider does, and what
an app killed in the middle of a debug session leaves behind.

Used by run.py. A held development build (MONOCODE_CODE_SELFTEST_HOLD names a
folder) writes the environment its session's agent process would get; this
starts `monocode-gpui code-mcp` with it and speaks MCP over stdio. What the
app shows meanwhile is asked of the app through files in that folder.
"""
import json
import os
import re
import signal
import subprocess
import time

import run


class Held:
    """The app held open, and the way to ask it what it shows."""

    def __init__(self, tag):
        self.folder = f"{run.ROOT}/hold-{tag}"
        os.makedirs(self.folder, exist_ok=True)
        for name in os.listdir(self.folder):
            os.remove(f"{self.folder}/{name}")
        self.result = f"{self.folder}/result.json"
        self.app = run.start_app(
            {
                "MONOCODE_CODE_SELFTEST": self.result,
                "MONOCODE_CODE_SELFTEST_SHOTS": run.SHOTS,
                "MONOCODE_CODE_SELFTEST_HOLD": self.folder,
            },
            f"{self.folder}/app.log",
        )
        self.n = 0
        env = self._wait(f"{self.folder}/env.json", 60)
        if env is None or "error" in env:
            raise RuntimeError(f"the app gave no environment: {env}")
        self.environment = env["environment"]
        self.session = env["session"]

    def _wait(self, path, seconds):
        for _ in range(seconds * 5):
            time.sleep(0.2)
            if self.app.poll() is not None:
                return None
            try:
                return json.load(open(path))
            except (OSError, ValueError):
                continue
        return None

    def ask(self, cmd, **extra):
        self.n += 1
        answer = f"{self.folder}/answer.json"
        if os.path.exists(answer):
            os.remove(answer)
        tmp = f"{self.folder}/ask.tmp"
        json.dump({"id": self.n, "cmd": cmd, **extra}, open(tmp, "w"))
        os.rename(tmp, f"{self.folder}/ask.json")
        for _ in range(300):
            time.sleep(0.2)
            try:
                got = json.load(open(answer))
            except (OSError, ValueError):
                continue
            if got.get("id") == self.n:
                return got.get("result")
        return None


class Tools:
    """`monocode-gpui code-mcp`, started with the session's environment."""

    def __init__(self, held):
        self.proc = subprocess.Popen(
            [run.APP, "code-mcp"],
            env=dict(os.environ, **held.environment),
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            text=True,
        )
        self.n = 0
        self.rpc("initialize", {"protocolVersion": "2025-03-26"})
        self.names = [t["name"] for t in self.rpc("tools/list")["result"]["tools"]]

    def rpc(self, method, params=None):
        self.n += 1
        message = {"jsonrpc": "2.0", "id": self.n, "method": method, "params": params or {}}
        self.proc.stdin.write(json.dumps(message) + "\n")
        self.proc.stdin.flush()
        return json.loads(self.proc.stdout.readline())

    def call(self, name, **arguments):
        """`(is_error, text)` of a tool call."""
        reply = self.rpc("tools/call", {"name": name, "arguments": arguments})
        if "error" in reply:
            return True, json.dumps(reply["error"])
        result = reply["result"]
        text = "\n".join(part.get("text", "") for part in result["content"])
        return bool(result.get("isError")), text

    def close(self):
        try:
            self.proc.stdin.close()
            self.proc.wait(5)
        except Exception:
            self.proc.kill()


def step(steps, number, name, ok, detail):
    steps.append({"step": number, "name": name, "pass": bool(ok), "detail": detail})
    print(f"{'PASS' if ok else 'FAIL'}  {number:>4}  {name}")
    if not ok:
        print("          " + json.dumps(detail)[:1800])


def stopped_at(text, name, line):
    return name in text and re.search(rf"{re.escape(name)}\D{{0,3}}{line}\b", text) is not None


def agent_phase(before):
    """Step 11, then step 12 with the app ended by SIGTERM mid-session."""
    steps = []
    held = Held("agent")
    tools = None
    try:
        tools = Tools(held)
        want = {"code_diagnostics", "code_definition", "run_start", "debug_start",
                "debug_variables", "debug_step", "debug_continue"}
        step(steps, "11a", "the code tools are offered to the session's agent",
             want <= set(tools.names), {"tools": tools.names})

        err, text = tools.call("code_diagnostics", files=["shop/orders.py"], timeout_s=60)
        step(steps, "11b", "code_diagnostics reports the type error on line 5 and the unused import on line 2",
             not err and re.search(r"\b5\b", text) and "three" in text.lower() + text
             and re.search(r"\b2\b", text) and ("F401" in text or "os" in text), {"text": text[:1500]})

        err, text = tools.call("code_definition", file="main.py", line=3, symbol="make_order")
        step(steps, "11c", "code_definition finds make_order in shop/orders.py at line 8",
             not err and "shop/orders.py" in text and re.search(r"\b8\b", text), {"text": text[:800]})

        err, text = tools.call("run_start", command="$PYTHON main.py", wait_s=60)
        state = held.ask("state")
        listed = [r for r in (state or {}).get("runs", []) if r["by"].startswith("Agent")]
        step(steps, "11d", "run_start with wait returns exit 0 and the output, and the app lists the run",
             not err and "order total 42.0" in text and "exited with code 0" in text
             and listed, {"text": text[:800], "runs_in_app": (state or {}).get("runs")})

        err, text = tools.call("debug_start", file="main.py", command="$PYTHON main.py",
                               breakpoints=[{"file": "main.py", "line": 9}], timeout_s=90)
        step(steps, "11e", "debug_start stops at the breakpoint on line 9 of main.py with the locals",
             not err and "main.py" in text and re.search(r"\b9\b", text) and "total" in text,
             {"text": text[:1500]})
        ref = re.search(r"order[^\n]*ref=(\d+)", text)
        if ref:
            err, expanded = tools.call("debug_variables", reference=int(ref.group(1)))
        else:
            err, expanded = True, "no ref=N for order in the stop report"
        step(steps, "11f", "debug_variables expands the order object's attributes",
             not err and "quantity" in expanded and "unit_price" in expanded, {"text": expanded[:800]})
        err, text = tools.call("debug_step", kind="over", timeout_s=60)
        step(steps, "11g", "debug_step over moves to line 10",
             not err and re.search(r"\b10\b", text), {"text": text[:800]})
        err, text = tools.call("debug_continue", timeout_s=60)
        step(steps, "11h", "debug_continue runs the program to its end with exit 0",
             not err and re.search(r"exit\D{0,12}0|ended|finished", text.lower()) and "done 42.0" in text,
             {"text": text[:800]})

        # A second session, left stopped for the app to show and the user to take over.
        err, text = tools.call("debug_start", file="main.py", command="$PYTHON main.py",
                               breakpoints=[{"file": "main.py", "line": 9}], timeout_s=90)
        time.sleep(1.5)
        state = held.ask("state")
        live = [s for s in (state or {}).get("sessions", []) if s["state"] == "paused"]
        agent_ones = [s for s in live if s["started_by_agent"]]
        shown = (
            bool(agent_ones) and state["debug_tab_drawn"] is not None
            and bool(state.get("rail_status")) and "main.py" in (state.get("rail_status") or "")
            and any(s["tab_open"] for s in agent_ones)
        )
        held.ask("shot", name="agent-debug-session")
        step(steps, "11i", "while the agent's session is stopped the app has a tab for it, marked as the agent's, and the rail status is set",
             shown, {"agent_text": text[:300], "app": state})
        taken = held.ask("take_over")
        driven = [s for s in (taken or {}).get("sessions", []) if s["state"] == "paused"]
        user_now = bool(driven) and all(s["user_drives"] for s in driven)
        err, refusal = tools.call("debug_step", kind="over", timeout_s=10)
        held.ask("shot", name="agent-session-taken-over")
        step(steps, "11j", "Take over (a click) makes the session the user's and the agent's next debug_step is refused",
             taken and taken.get("clicked") and user_now and err, {"taken": taken, "refusal": refusal[:600]})
        # Step 12: the app ended by SIGTERM in the middle of the session.
        held.app.send_signal(signal.SIGTERM)
        try:
            held.app.wait(10)
            exited = True
        except subprocess.TimeoutExpired:
            exited = False
        time.sleep(2)
        left = run.leftovers(before)
        step(steps, "12b", "SIGTERM in the middle of a debug session leaves no language server, adapter or debuggee behind",
             exited and not left, {"app_exited": exited, "left": left})
        run.sweep(before)
    finally:
        if tools:
            tools.close()
        run.end(held.app)
    return steps


def kill_phase(before):
    """Step 12 with SIGKILL: what survives is reported, not required to be nothing."""
    steps = []
    held = Held("kill")
    tools = None
    try:
        tools = Tools(held)
        err, text = tools.call("debug_start", file="main.py", command="$PYTHON main.py",
                               breakpoints=[{"file": "main.py", "line": 9}], timeout_s=90)
        tools.call("code_diagnostics", files=["shop/orders.py"], timeout_s=60)
        held.app.send_signal(signal.SIGKILL)
        held.app.wait(10)
        time.sleep(2.5)
        left = run.leftovers(before)
        step(steps, "12c", "SIGKILL mid-session: what survives (known limit; reported, swept afterwards)",
             True, {"started": not err, "survivors": left})
        run.sweep(before)
    finally:
        if tools:
            tools.close()
        run.end(held.app)
    return steps
