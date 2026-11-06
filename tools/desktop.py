#!/usr/bin/env python3
"""Build/check the sibling desktop repositories without an installed compositor."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import re
import shutil
import tempfile

WM = Path(__file__).resolve().parents[1]
ROOT = WM.parent
SATELLITE = ROOT / "xwayland-satellite"
CONFIG = ROOT / "shojiwm-config"

def run(args, cwd=WM, env=None):
    print("+", " ".join(map(str, args)), flush=True)
    subprocess.run(list(map(str, args)), cwd=cwd, env=env, check=True)

def cargo_env(repo):
    env = os.environ.copy()
    env["CARGO_HOME"] = env.get("DESKTOP_CARGO_HOME", str(Path.home() / ".cargo"))
    env["CARGO_TARGET_DIR"] = str(repo / "target")
    # V8's versioned upstream release is selected by Cargo.lock/build.rs.
    # Historical task-local archive overrides must not leak into this build.
    env.pop("RUSTY_V8_ARCHIVE", None)
    if env.get("DESKTOP_V8_ARCHIVE"):
        env["RUSTY_V8_ARCHIVE"] = env["DESKTOP_V8_ARCHIVE"]
    return env

def cargo(repo, *args):
    offline = ["--offline"] if os.environ.get("DESKTOP_OFFLINE") == "1" else []
    run(["cargo", *args, *offline, "--locked", "-j", os.environ.get("DESKTOP_BUILD_JOBS", "8")], repo, cargo_env(repo))

def build():
    cargo(WM, "build", "--release", "-p", "shoji_wm")
    cargo(SATELLITE, "build", "--release")

def satellite_test():
    # Use the checked-in lockfile unchanged and give each real-Xwayland case
    # a fresh process/runtime, so process-global test state cannot leak.
    with tempfile.TemporaryDirectory(prefix="satellite-tests-") as temp:
        env = cargo_env(SATELLITE)
        env.update(XDG_RUNTIME_DIR=temp, RUST_TEST_THREADS="1", LIBGL_ALWAYS_SOFTWARE="1")
        for key in ("DISPLAY", "WAYLAND_DISPLAY", "WAYLAND_SOCKET", "SHOJIWM_SOCKET", "DBUS_SESSION_BUS_ADDRESS"):
            env.pop(key, None)
        flags = (["--offline"] if os.environ.get("DESKTOP_OFFLINE") == "1" else []) + ["--locked", "-j", "8"]
        run(["cargo", "test", "--lib", *flags], SATELLITE, env)
        built = subprocess.run(["cargo", "test", "--test", "integration", "--no-run", "--message-format=json", *flags], cwd=SATELLITE, env=env, stdout=subprocess.PIPE, text=True, check=True)
        binaries = [message["executable"] for line in built.stdout.splitlines() if (message := json.loads(line)).get("reason") == "compiler-artifact" and message.get("target", {}).get("name") == "integration" and message.get("executable")]
        if len(binaries) != 1:
            raise RuntimeError("Expected one integration test executable")
        binary = binaries[0]
        listing = subprocess.check_output([binary, "--list"], env=env, text=True)
        cases = [line.removesuffix(": test") for line in listing.splitlines() if line.endswith(": test")]
        if not cases:
            raise RuntimeError("Integration test list was empty")
        for name in cases:
            with tempfile.TemporaryDirectory(prefix="satellite-case-") as runtime:
                case_env = env.copy()
                case_env["XDG_RUNTIME_DIR"] = runtime
                print("Integration case:", name, flush=True)
                run([binary, "--exact", name], SATELLITE, case_env)
        print(f"All {len(cases)} integration cases passed in fresh processes", flush=True)

def wm_test():
    env = cargo_env(WM)
    result = subprocess.run(["cargo", "test", *(["--offline"] if os.environ.get("DESKTOP_OFFLINE") == "1" else []), "--locked", "-j", "8", "-p", "shojiwm_lib", "-p", "shoji_wm", "--lib", "--no-run", "--message-format=json"], cwd=WM, env=env, stdout=subprocess.PIPE, text=True, check=True)
    binaries = []
    for line in result.stdout.splitlines():
        message = json.loads(line)
        if message.get("reason") == "compiler-artifact" and message.get("profile", {}).get("test") and message.get("executable"):
            binaries.append(message["executable"])
    if not binaries:
        raise RuntimeError("Cargo returned no test executables")
    for binary in binaries:
        run(["python3", WM / "tests/run-native-tests.py", binary], env=env)

def config_check():
    run(["npm", "ci", *(["--offline"] if os.environ.get("DESKTOP_OFFLINE") == "1" else []), "--ignore-scripts", "--no-audit", "--no-fund"], WM)
    modules = CONFIG / "shojiwm/node_modules"
    modules.mkdir(exist_ok=True)
    link = modules / "shoji_wm"
    sdk = WM / "packages/shoji_wm"
    if link.is_symlink():
        link.unlink()
    elif link.exists():
        raise RuntimeError(f"Refusing to replace a real SDK directory: {link}")
    link.symlink_to(os.path.relpath(sdk, modules), target_is_directory=True)
    run(["node", WM / "node_modules/typescript/bin/tsc", "--noEmit", "-p", CONFIG / "shojiwm/tsconfig.json"])
    env = os.environ.copy()
    env["SHOJI_TEST_TYPESCRIPT"] = str(WM / "node_modules/typescript")
    run(["node", CONFIG / "shojiwm/tests/run.cjs"], CONFIG, env)
    run(["node", "--test", CONFIG / "steampunk-preview/tests/metrics.test.cjs"], CONFIG)
    run(["python3", "-m", "unittest", "discover", "-s", CONFIG / "steampunk-preview/tests", "-p", "test_*.py"], CONFIG)


def git_check():
    for repo in (WM, SATELLITE, CONFIG):
        common = subprocess.check_output(["git", "-C", str(repo), "rev-parse", "--path-format=absolute", "--git-common-dir"], text=True).strip()
        if Path(common).resolve() != repo / ".git":
            raise RuntimeError(f"External shared Git directory: {repo}")
        if (repo / ".git/objects/info/alternates").exists():
            raise RuntimeError(f"External object store: {repo}")
        remote = subprocess.check_output(["git", "-C", str(repo), "remote", "get-url", "origin"], text=True).strip()
        if not remote.startswith("https://github.com/"):
            raise RuntimeError(f"Unexpected non-public origin: {repo}")
        run(["git", "fsck", "--full"], repo)

def isolated():
    if not shutil.which("bwrap"):
        raise RuntimeError("Install bubblewrap to run the optional old-path isolation check")
    command = ["bwrap", "--unshare-net", "--ro-bind", "/", "/", "--dev-bind", "/dev", "/dev", "--bind", str(ROOT), str(ROOT), "--tmpfs", "/tmp", "--chmod", "1777", "/tmp", "--dir", "/tmp/.X11-unix", "--chmod", "1777", "/tmp/.X11-unix"]
    for cache in (Path.home() / ".cargo", Path.home() / ".npm"):
        if cache.is_dir():
            command += ["--bind", str(cache), str(cache)]
    # These mounts affect only the child. No original directory is moved.
    for old in ROOT.parent.iterdir():
        if old.is_dir() and re.fullmatch(r"\d{4}-\d{2}-\d{2}", old.name):
            command += ["--tmpfs", str(old)]
    for old in (Path.home() / "Documents/Claude", Path.home() / "Documents/Git/ShojiWM", Path("/usr/lib/shojiwm")):
        if old.is_dir():
            command += ["--tmpfs", str(old)]
    child_action = os.environ.get("DESKTOP_ISOLATED_ACTION", "all")
    if child_action not in ("all", "git-check", "build", "wm-test", "satellite-test", "config-check"):
        raise RuntimeError("Invalid DESKTOP_ISOLATED_ACTION")
    command += ["--setenv", "DESKTOP_OFFLINE", "1", "--chdir", str(WM), "python3", str(WM / "tools/desktop.py"), child_action]
    run(command)

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=["build", "wm-test", "satellite-test", "config-check", "git-check", "isolated", "all"])
    action = parser.parse_args().action
    for name, function in [("git-check", git_check), ("isolated", isolated), ("build", build), ("wm-test", wm_test), ("satellite-test", satellite_test), ("config-check", config_check)]:
        if action == name or (action == "all" and name != "isolated"):
            function()
