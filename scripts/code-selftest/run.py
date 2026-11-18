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


def fresh_world():
    for name in ("data", "home"):
        shutil.rmtree(f"{ROOT}/{name}", ignore_errors=True)
        os.makedirs(f"{ROOT}/{name}")
    subprocess.run([sys.executable, f"{HERE}/sample.py", PROJECT, VENV], check=True)


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


def sweep(before):
    """Ends whatever a self-test left behind; returns what that was."""
    left = {pid: cmd for pid, cmd in processes().items() if pid not in before}
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
    parser.add_argument("--only", default="")
    parser.add_argument("--skip-shutdown", action="store_true")
    args = parser.parse_args()
    if not os.path.exists(APP):
        sys.exit(f"{APP} is not built; see README.md")

    before = processes()
    if before:
        print("already running, left alone:", before)
    shutil.rmtree(SHOTS, ignore_errors=True)
    os.makedirs(SHOTS)
    fresh_world()

    result = f"{ROOT}/result.json"
    if os.path.exists(result):
        os.remove(result)
    extra = {
        "MONOCODE_CODE_SELFTEST": result,
        "MONOCODE_CODE_SELFTEST_SHOTS": SHOTS,
        "MONOCODE_CODE_SELFTEST_ONLY": args.only,
    }
    app = start_app(extra, f"{ROOT}/app.log")
    try:
        report = wait_for(result, app, 600)
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
    if not report.get("finished"):
        print("NOT FINISHED; the app's log ends with:")
        print("".join(open(f"{ROOT}/app.log").readlines()[-25:]))
    print(f"{'PASS' if quit_itself and not left else 'FAIL'}   12a quitting leaves no process behind",
          json.dumps({"quit_by_itself": quit_itself, "left": left}))
    print(f"report: {result}\npictures: {SHOTS}")


if __name__ == "__main__":
    main()
