# Code self-test

Run, code insight and debug checked in the real window of the development
build. The app drives itself through its own event path (real mouse and key
events), the agents' tools are used through `monocode-gpui code-mcp`, and the
window is saved as PNGs (drawn again off screen: no screen recording
permission needed).

Everything happens under `/private/tmp/code-selftest`: a sample Python
project (`sample.py`), the app's data directory (`MONOCODE_NATIVE_DATA_DIR`),
the pictures in `shots/` and the reports. It never touches your data or the
installed SOCode, and it ends every process it started (matched by the sample
project's path; other processes are left alone).

    cargo build --manifest-path crates/monocode-gpui/Cargo.toml -p monocode-gpui \
        --profile native-dev --features selftest
    python3 scripts/code-selftest/run.py                 # everything, ~6 minutes
    python3 scripts/code-selftest/run.py --only run,debug

`--only` takes `insight,run,debug,idle` (in the window), `nodebugpy` (a second
project whose environment has no debugpy), `agent` (steps 11 and 12 with
SIGTERM) and `sigkill`. The reports are `result.json` and `report.json`.

Needs `~/Dev/socode-ide-fixture/.venv` (basedpyright, ruff, debugpy, pytest;
read only; override with `MONOCODE_SELFTEST_VENV`) and `uv`. The code is
behind the cargo feature `selftest`, off by default, so the ordinary and
release builds contain none of it. See docs/native/code.md section 8.

Files: `run.py` (the world and the phases), `agent.py` (the tools and the
shutdowns), `sample.py` (the sample project), and in the app
`shell/code_selftest.rs` with `code_selftest/`.
