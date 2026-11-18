#!/usr/bin/env python3
"""Runs the code self-test of the development build in a world of its own.

    run.py [--only insight,run,debug,agent,idle] [--skip-shutdown] [--keep]

Everything lives under /private/tmp/code-selftest: the sample project, the
app's data directory, the pictures and the reports. The app is started with
MONOCODE_NATIVE_DATA_DIR set to that data directory and never touches the
installed app's. Every process started here is ended before it returns.
See README.md.
"""
import argparse
import json
import os
import shutil
import signal
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
APP = os.environ.get(
    "MONOCODE_SELFTEST_APP",
    os.path.join(REPO, "crates/monocode-gpui/target/native-dev/monocode-gpui"),
)
VENV = os.environ.get(
    "MONOCODE_SELFTEST_VENV", os.path.expanduser("~/Dev/socode-ide-fixture/.venv")
)
ROOT = "/private/tmp/code-selftest"
PROJECT = f"{ROOT}/proj"
SHOTS = f"{ROOT}/shots"

# What a process of ours has in its command line: the sample project (the
# interpreter and the servers are started from its `.venv` link, so their
# paths start with it). Nothing that merely uses the fixture environment is
# matched: other work on this machine does, and is not ours to end.
MARKS = [ROOT + "/"]


def processes():
    """`{pid: command}` of every process that belongs to a self-test."""
    listing = subprocess.run(
        ["ps", "-axo", "pid=,command="], capture_output=True, text=True
    ).stdout
    found = {}
    for line in listing.splitlines():
        pid, _, command = line.strip().partition(" ")
        if pid.isdigit() and int(pid) != os.getpid() and any(m in command for m in MARKS):
            if "run.py" in command or "agent.py" in command:
                continue
            found[int(pid)] = command.strip()
    return found


def fresh_world(venv=VENV):
    for name in ("data", "home"):
        shutil.rmtree(f"{ROOT}/{name}", ignore_errors=True)
        os.makedirs(f"{ROOT}/{name}")
    subprocess.run([sys.executable, f"{HERE}/sample.py", PROJECT, venv], check=True)


def venv_without_debugpy():
    """A virtual environment with nothing in it, for the missing-adapter step."""
    path = f"{ROOT}/venv-nodebugpy"
    shutil.rmtree(path, ignore_errors=True)
    subprocess.run(["uv", "venv", "-q", path], check=True)
    return path


def start_app(extra, log):
    env = dict(
        os.environ,
        MONOCODE_DRAW_HIDDEN="1",
        RUST_BACKTRACE="1",
        MONOCODE_NATIVE_DATA_DIR=f"{ROOT}/data",
        **extra,
    )
    return subprocess.Popen(
        [APP, "--dev-data", "--cwd", PROJECT],
        env=env,
        stdout=open(log, "w"),
        stderr=subprocess.STDOUT,
    )


def wait_for(path, app, seconds, done=lambda report: report.get("finished")):
    """The report at `path` once `done`, or what there is when time is up."""
    report = {}
    for _ in range(seconds * 2):
        time.sleep(0.5)
        try:
            report = json.load(open(path))
        except (OSError, ValueError):
            report = {}
        if done(report) or app.poll() is not None:
            break
    return report


def end(app, how=signal.SIGTERM, wait=8):
    if app.poll() is None:
        app.send_signal(how)
        try:
            app.wait(wait)
        except subprocess.TimeoutExpired:
            app.kill()
            app.wait()


def leftovers(before):
    """What a self-test started and has not ended, without ending it."""
    return {pid: cmd for pid, cmd in processes().items() if pid not in before}


def sweep(before):
    """Ends whatever a self-test left behind; returns what that was."""
    left = leftovers(before)
    for pid in left:
        try:
            os.kill(pid, signal.SIGKILL)
        except OSError:
            pass
    return left


def show(report):
    for step in report.get("steps", []):
        print(f"{'PASS' if step['pass'] else 'FAIL'}  {step['step']:>3}  {step['name']}")
        if not step["pass"]:
            print("          " + json.dumps(step["detail"])[:1500])
    failed = [p for p in report.get("pictures", []) if "error" in p]
    if failed:
        print(f"pictures: {len(failed)} failed, e.g. {failed[0]}")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--only",
        default="",
        help="comma list of insight,run,debug,idle (in the window), nodebugpy, agent, sigkill",
    )
    args = parser.parse_args()
    if not os.path.exists(APP):
        sys.exit(f"{APP} is not built; see README.md")
    wanted = [part for part in args.only.split(",") if part]
    in_app = [p for p in wanted if p in ("insight", "run", "debug", "idle")]
    do = lambda part: not wanted or part in wanted
    import agent

    before = processes()
    if before:
        print("already running, left alone:", before)
    shutil.rmtree(SHOTS, ignore_errors=True)
    os.makedirs(SHOTS)
    steps = []

    def window(only, venv=VENV):
        """One run of the app in its window, driving itself through `only`."""
        out = []
        fresh_world(venv)
        result = f"{ROOT}/result.json"
        if os.path.exists(result):
            os.remove(result)
        extra = {
            "MONOCODE_CODE_SELFTEST": result,
            "MONOCODE_CODE_SELFTEST_SHOTS": SHOTS,
            "MONOCODE_CODE_SELFTEST_ONLY": ",".join(only),
        }
        app = start_app(extra, f"{ROOT}/app.log")
        try:
            report = wait_for(result, app, 900)
            # It quits by itself; what is left then is step 12's first answer.
            try:
                app.wait(15)
            except subprocess.TimeoutExpired:
                pass
            quit_itself = app.poll() is not None
        finally:
            end(app)
        time.sleep(1.5)
        left = sweep(before)
        show(report)
        out += report.get("steps", [])
        if not report.get("finished"):
            print("NOT FINISHED; the app's log ends with:")
            print("".join(open(f"{ROOT}/app.log").readlines()[-25:]))
        ok = quit_itself and not left
        print(f"{'PASS' if ok else 'FAIL'}   12a quitting leaves no process behind",
              json.dumps({"quit_by_itself": quit_itself, "left": left}))
        out.append({"step": "12a", "name": "quitting leaves no process behind", "pass": ok,
                    "detail": {"quit_by_itself": quit_itself, "left": left}})
        return out

    if not wanted or in_app:
        steps += window(in_app)
    if do("nodebugpy"):
        steps += window(["nodebugpy"], venv_without_debugpy())

    if do("agent"):
        fresh_world()
        steps += agent.agent_phase(before)
    if do("sigkill"):
        fresh_world()
        steps += agent.kill_phase(before)

    json.dump({"steps": steps}, open(f"{ROOT}/report.json", "w"), indent=1)
    print(f"report: {ROOT}/result.json and {ROOT}/report.json\npictures: {SHOTS}")
    stray = leftovers(before)
    if stray:
        print("STILL RUNNING after the self-test:", stray)
        sweep(before)


if __name__ == "__main__":
    main()
