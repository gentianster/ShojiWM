# Desktop sources and checks

Keep these sibling checkouts together: `shojiwm`, `xwayland-satellite`, and
`shojiwm-config`. All three have their own `.git`; local history and configuration
changes do not require an older task folder. Remotes point to the published forks.
The compositor's Cargo.lock pins the reviewed satellite commit; ordinary builds
use that exact Git dependency, not an uncommitted sibling path override.

From `shojiwm` run:

```sh
python3 tools/desktop.py build
python3 tools/desktop.py wm-test
python3 tools/desktop.py satellite-test
python3 tools/desktop.py config-check
# Or run all four in order:
python3 tools/desktop.py all
```

Use system Cargo/Rust (validated with 1.93.1), Node.js 22, npm, Python 3,
C/C++ build tools, pkg-config, and the development libraries required by the
upstream README/Nix flake. The multiline dependency tables are formatted for
system Cargo without changing dependency versions. Cargo commands use checked-in
lockfiles. `npm ci` uses package-lock.json. Network access is needed for uncached
locked dependencies and the versioned rusty_v8 release selected by its build.rs.
No dated compiler, V8 archive, old build target or installed SDK is required.

Outputs go only to each source repo's ignored `target/` or `node_modules/`.
Cargo's ordinary dependency cache is `$HOME/.cargo`; optional
`DESKTOP_CARGO_HOME` may select another non-task cache. Set `DESKTOP_BUILD_JOBS`
to change the default 8 build jobs. `DESKTOP_V8_ARCHIVE` is an optional explicit
V8 release archive override; no historical path is assumed.

`config-check` points only the settings checkout's ignored SDK symlink at the
sibling SDK. It does not change `~/.config/shojiwm`, `/usr/lib/shojiwm` or the live
SDK. The deployment package.json continues to describe its installed SDK.
Rust tests get private runtime directories; satellite integration tests start
only their own Xwayland. Optional visual shell tests additionally need Xvfb,
xdotool, Quickshell, ffmpeg and the test helper described in the config README.
These commands never install a compositor, reload a config, restart a session,
or push a branch. Build outputs can be regenerated; they are not source backups.

Optional proof that old task folders are unnecessary (after the initial fetch/
build fills the ordinary caches):

```sh
python3 tools/desktop.py git-check
python3 tools/desktop.py isolated
```

The latter needs bubblewrap. It repeats all checks offline with old date folders,
the former Claude build location, old source clone, and installed SDK masked only
inside a child mount namespace. It also gives Xwayland a private `/tmp` and
network namespace. The host directories and running compositor are untouched.
`DESKTOP_OFFLINE=1` can also be used for ordinary commands after dependencies are
cached. The optional GUI suite is not included: it needs the additional system
tools listed in the configuration repo's tools/README.md.

Satellite integration cases run once each in fresh processes and private runtime
directories, avoiding shared process-global test state. Failures stop the command;
the runner does not hide failures with automatic retries.

Known test limitation: existing satellite clipboard event-wait tests may fail
intermittently at the pinned source revision. Commands report failures directly;
this build setup does not claim to fix clipboard behavior or retry until success.
