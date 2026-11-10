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

## Deployed binaries (gebuntu)

### Reproduction matrix

- **shoji_wm** (compositor)
  - Installed at: `/usr/bin/shoji_wm`
  - sha256: `6cbe48afa897195037ccbbd1fbf6da6e385854d26d11e123b527fda2e5803f10`
  - Source: gentianster/ShojiWM `b947f690b29fae64fe2397c1fbecfb7fe758d578`
    (main; merge of PR #5, session-lock commit-timing fix `aae6391`)
  - Built on gebuntu with system cargo/rustc 1.93.1 (see the toolchain note above).
  - Live-verified by the user on 2026-11-07 (lock screen locks and unlocks).
- **xwayland-satellite** (build-time dependency of the compositor)
  - Pinned in `Cargo.lock` to gentianster/xwayland-satellite
    `06511b6d08665ed55f4966f2e24fa387e58c9d17` (version 0.8.3).
  - Builds use this lockfile pin. Known limitation: `tools/desktop.py satellite-test`
    exercises the sibling `xwayland-satellite` checkout
    (`SATELLITE = ROOT / "xwayland-satellite"`), not the pinned revision, so its
    results do not cover the pin. This is a known gap; this change does not alter
    desktop.py.
- **xdg-desktop-portal-shojiwm** (portal backend)
  - Installed at: `/usr/bin/xdg-desktop-portal-shojiwm`
  - sha256: `e41956bdf07c3dc687e5d4adaaeafec42e7c7b3df1e6ca95f0029f638916cb69`
  - Source: gentianster/ShojiWM `8ca5abe9672e66edf313d432ed03407bea470c6e`
    (PR #7, preview resource cleanup and source picker improvements).
  - Built with system Cargo/Rust 1.93.1 using
    `CARGO_BUILD_JOBS=4 cargo build --release --locked -p xdg-desktop-portal-shojiwm --bin xdg-desktop-portal-shojiwm`.
  - Deployed on 2026-11-10 at 17:41 JST (27,135,400 bytes). The staged binary
    was hash-verified and atomically renamed into place; only the idle ShojiWM
    portal backend was restarted. The compositor, frontend portal, GTK backend,
    service definitions and user configuration were unchanged.
  - Verified the running executable hash and ScreenCast source types through
    both backend and frontend D-Bus interfaces. Three live cancellation cases
    returned promptly; each released the thumbnail worker and thumbnail memfds.
    A synthetic-window stream node was created and closed cleanly. The local
    GStreamer validation client returned `target not found`, so end-to-end frame
    reception in Chrome/OBS remains unverified; this is not a streaming pass.
  - Rollback binary:
    `/usr/bin/.xdg-desktop-portal-shojiwm.before-preview-fix-20261110`.
    Its sha256 is `1fc25cb2939ab42f770b8330446b087f0590ee57717ad000d3b725d2a747ebd3`;
    its original source revision is unknown. Restore it through a staged atomic
    rename and restart only the backend when no sharing sessions are active.
  - Source in this repo: `src/xdg-desktop-portal-shojiwm`; install assets in
    `dist/` (`install-portal.sh`, service files).

### Deploy convention for /usr/bin/shoji_wm

- Stage the new binary, verify its sha256, then atomically rename it into place
  with `mv -T` (a running session keeps the old inode).
- Keep the replaced binary as `/usr/bin/.shoji_wm.before-<name>`.
- Current backups on gebuntu: `.before-session-lock-fix-20261107`
  (sha256 `46f6dba6b2a8696b0b182ef96e21043cb2a3e30de75715ff53b3c37c642270cd`),
  `.before-satellite-pin-20261107`, `.before-integration-20261107`,
  `.before-rebuild-20261107`, `.before-numlock`, `.before-fullscreen-fix`,
  `.before-bookmark-position-fix`.

### Standing rule

- Every deploy updates this section in the same change: source revision, sha256,
  and date are recorded at deploy time, for the portal as well. This rule exists
  because the portal's October deploy went unrecorded.
