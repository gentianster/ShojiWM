# ShojiWM upstream integration candidate

Date: 2026-11-10 (JST). Branch: `integration/upstream-20261110`.

## Scope and checkpoints

- Fork base: `e6d5b187f137e5a256a6d4ce984faedbe22d8f71` (`origin/main`).
- Upstream baseline: `2ae3e7c9a412c4656a3908fedc46637d8ec79c79` ([source](https://github.com/bea4dev/ShojiWM/commit/2ae3e7c9a412c4656a3908fedc46637d8ec79c79)).
- Common ancestor: `98f84845ebbaabf0cd847d7dc5ac628e5adfccce`.
- Stage 1: merge `52615c3277d2faca82f9313432d4e22e77507bcd`, upstream plus fork-specific changes. Its tree is identical to the originally verified snapshot `e71fc2753dca014b8cfc483afd46b3a31e8c4fc5`.
- Stage 2: `030c7f9246b0fb9e4238ca1a0c74baff337facb1`, with the exact tree previously verified at `167693183a041edcf94becebeb8c4d7d515977b0`. The following portal changes adapt [PR #7](https://github.com/gentianster/ShojiWM/pull/7), implementation `8ca5abe9672e66edf313d432ed03407bea470c6e`, to that verified baseline.
- Stage 1 is now a proper two-parent **local integration-branch merge**, with fork `e6d5b18` as first parent and upstream `2ae3e7c` as second parent. Both are confirmed ancestors. Main and PR refs remain unchanged.
- No push, PR update, merge into main, installed-file replacement, portal restart or desktop restart was performed for this candidate.

The original PR branch, including its local deployment record, remains at `e452e6e0bb382c1fd28de1f936f8fc88c1bfc4cc`. `DESKTOP-BUILD.md` retains that record and describes the previously installed PR #7 build, not this candidate.

## Stage 1: conflict resolutions

The fork has 16 unique commits and upstream has 71 since the common ancestor. Five files each had one textual conflict:

| File | Resolution |
| --- | --- |
| `src/shojiwm_lib/Cargo.toml` | Use upstream Smithay `c1253794ee2a7c466e88bb20dfdaabb4ccbfb2aa`; retain the fork satellite `06511b6d08665ed55f4966f2e24fa387e58c9d17` and standard Cargo-compatible inline-table syntax. |
| `src/shojiwm_lib/src/handlers/compositor.rs` | Preserve upstream close-snapshot capture before destruction and fork XWayland position invalidation. |
| `src/shojiwm_lib/src/handlers/mod.rs` | Preserve upstream session-lock protection and fork position publication before focus. |
| `src/shojiwm_lib/src/input.rs` | Preserve position publication together with upstream locked VT-switch behavior. |
| `src/shojiwm_lib/src/state.rs` | Preserve position publication before focus while adopting upstream updates. |

The stage-one portal picker, source discovery and screencast files were byte-for-byte upstream, so the two stages can be inspected independently.

## Fork-only commits: 16 total, 11 ordinary and 5 merge wrappers

- `b8a55686950f09276c3703a55f4bf58ec2b16ad7` - Pin the verified emulator utility parent fix
- `226ea373e634e20e7e92a5e13cae001fb4a670b5` - Keep DPI stabilization and popup coordinates in the pinned satellite
- `bc81279a535f26df326e1200fccf003c11e90ad0` - Allow embedded satellite compositor scaling through session environment
- `7dcab0e096f836bd72caaf895d1ce606de688ac0` - Pin optional compositor scaling and retain verified popup fixes
- `92255f7120f2ffc25d3cc10fa45d1619dfaa577a` - Make desktop builds reproducible without historical task folders
- `007a32c8452a7437493163dd1aac6cd0bc6723a2` - Pin the satellite fullscreen output membership fix
- `69ccec1da9682d830c38c6232c1432d411fd0cdc` - Synchronize embedded Xwayland surface positions before input
- `4cb25b669dbd98a8b322731b4b9ffa05999189e8` - start sessions with num lock on
- `5f3cdf6ab38ab1b4276fcde0a77b5b08afc2f38a` - Merge fix/android-emulator-popup-parent into main (integration candidate)
- `95255607cf074222500da41d8f5df2a677d4d51a` - Merge pull request #3 from gentianster/integrate/android-emulator-popup-parent
- `4ebd2760b3f34ec994702ff48e5914aecb8d75ea` - Bump embedded xwayland-satellite pin to 06511b6 (0.8.3)
- `701922cc461b08903ca9d3c86723d2f6a4c6cc6d` - Merge pull request #4 from gentianster/fix/satellite-pin-06511b6
- `aae6391d4235413dae97f04c6232864b4ae221d3` - signal commit timing barriers for session lock surfaces
- `b947f690b29fae64fe2397c1fbecfb7fe758d578` - Merge pull request #5 from gentianster/fix/session-lock-commit-timing
- `b6bdbae3ea36d0edc67aaa4ce23a12162daf4293` - docs: record deployed binary provenance and deploy convention
- `e6d5b187f137e5a256a6d4ce984faedbe22d8f71` - Merge pull request #6 from gentianster/docs/deployed-binary-matrix

## Stage 2: upstream versus PR #7

| Concern | Decision |
| --- | --- |
| Frame/source/session/buffer/pool destruction and unmapping | Keep upstream cleanup behavior; add RAII ownership so early returns and unwinding also release resources. Preserve unmap-error warnings. |
| SHM allocation | Add one checked layout calculation for dimensions, stride and total size before allocation/protocol calls. |
| Thumbnail teardown | Keep upstream worker ownership and normal `spawn_blocking` teardown; add established-socket shutdown before join, race-safe socket registration and duplicate-FD cleanup. |
| Session lifecycle | Keep the unified upstream `SessionState`, watch cancellation, duplicate-session rejection and restoration rules. |
| Request attribution | Use a unique `SelectionRequestId` from `SelectSources` through thumbnail updates, picker commands and result insertion. Replacing a selection cancels the old watch atomically. Old snapshots/completions cannot affect a new attempt in the same session. Session Close still cancels the whole session. |
| Abandoned picker future | Add a request-scoped drop guard and reject closed/cancelled queued requests. |
| Discovery and refresh | Keep upstream open/close/title/hotplug updates, metadata, cache pruning, bounded two-snapshot queue and nonblocking sends. |
| Selection UI | Add trimmed, case-insensitive search; stable source-ID pending selection; a separate Share confirmation; Escape and guarded Enter; persistent Cancel and empty/no-match states; responsive previews and Japanese/English labels. Revalidate selection after source updates, rename, filter and tab changes. |

## Regression checklist and actual results

This checklist was recorded before source changes and then updated from completed commands. No retry-until-green was used.

| Area | Result and boundary |
| --- | --- |
| Locked workspace build | Stage 1 and stage 2 `cargo build --locked --workspace -j 4` passed. |
| All workspace targets | Stage 1: 459 distinct top-level tests passed. Stage 2: 489 distinct top-level tests passed. The synthetic demo separately reran 28 picker/source tests, so its duplicate cases are not included in 489. No failures. |
| Offscreen GPU | All four normally ignored GPU cases passed separately at both stages with surfaceless software EGL. This is not hardware-GPU validation. |
| Portal lifecycle and UI | 35 tests passed: 21 picker, 7 screencast and 7 source-lifecycle cases. Includes 30 new tests relative to upstream. Tests cover stale same-session updates/completions, future cancellation, request replacement, source removal/reorder/rename, explicit confirmation, early returns, SHM size bounds, 64 FD/mapping release cycles and interruption of a real blocked Wayland roundtrip over a private socket pair. |
| Formatting and lint | `cargo fmt --all -- --check` and `git diff --check` passed. Strict clippy passed for the portal binary and synthetic demo (`-D warnings`); no claim of workspace-wide strict clippy. |
| Exact satellite pin | 153 unit tests and 45 real-Xwayland integration cases passed at exact commit `06511b6`. No retries, timeouts or skipped cases. Its checkout stayed clean. |
| XWayland parent/popup/input | Exact-pin cases passed for parent priority, nested/sibling popup positions, native-position barriers, input ordering, fullscreen membership/last-pixel reachability, fixed/fractional/automatic DPI, output relocation/removal and clipboard. Fork compositor publication/invalidation hooks retained by source review. |
| NumLock | Startup enablement and keymap rebuild restoration/notification retained by source review. Fresh-session keypad/hotplug behavior remains a manual check after authorized deployment. |
| Session lock | Fork commit-timing barriers and upstream lock/VT/focus protections retained. C timing probe compiled and its no-argument safety refusal was verified. Actual PAM lock/unlock and runtime timing remain untested. |
| SDK | Typecheck, 8 overlay cases plus fixtures, and 4 output-power cases passed. Dependency lock unchanged. |
| Config repository | Candidate-SDK and existing-SDK typechecks passed for `Documents/Codex/desktop-customization/shojiwm-config/shojiwm`. 67 mocked cases passed: 25 workspace, 20 switcher, 16 overview and 6 QML transitions. Existing dirty files, status and SDK symlink remained unchanged. This tree differs from the actual running config; see the separate live-config check below. |
| Actual live config | `~/.config/shojiwm` passed installed-SDK baseline and candidate-SDK typechecks using isolated temporary compiler settings (TypeScript 5.9.3), including its live-only drag/drop and workspace-drum files. File-list verification confirmed candidate SDK paths and no installed SDK paths in the candidate check. Config/SDK hashes, symlinks and repository statuses remained unchanged. This was static checking only, not V8 execution. |
| Documentation | Typecheck and English/Japanese production builds passed; docs locks unchanged. |
| Nix | Not verified: evaluation with invocation-only `nix-command flakes` reached the daemon but failed with Permission denied on `/nix/var/nix/daemon-socket/socket`. No permissions, config, services or lockfiles were changed. |
| Visual picker QA | Japanese synthetic windows at widths 720 and 360 rendered successfully. Selected, empty and no-match states inspected; footer actions remain visible, long titles wrap, Share is disabled without valid selection. Only each demo's own renderer was saved, never desktop/source content. |
| Live sharing/hardware | Candidate not installed. Actual Chrome shared frames, hardware mixed-DPI behavior, fresh-session NumLock and PAM lock/unlock require separately authorized live validation. |

## Review, remaining edges and next decision

Final read-only review found no confirmed new blocking regression in the stage-two delta. The limitations below are retained explicitly rather than treated as validated behavior.

The thumbnail worker can interrupt an established Wayland connection. The initial `Connection::connect_to_env()` call occurs before socket registration and is not made interruptible by this patch. Thus a pathological stall during initial connection setup is not covered by the blocked-roundtrip test. Aborting the outer `SelectSources` future also drops its joining guard inline instead of reaching normal `spawn_blocking` teardown; this pre-existing path can still block during that setup edge. Tests cover picker-future abort and thumbnail interruption separately, not an end-to-end outer-future abort. Cancellation during actual asynchronous window opening has state-level coverage only, not a real UI event-order test.

The local integration history now records upstream ancestry correctly. Publishing this branch, merging it into main and installing it still require explicit decisions. Live validation should explicitly cover repeated picker open/cancel/share/stop, successful browser frame delivery, stable thumbnail FD/mapping counts after cancellation, NumLock, lock/unlock, and hardware mixed-DPI/XWayland popup positions. Preserve the existing installed-binary rollback plan.

## Local history repair after review

Muse reviewed the original candidate read-only and reported no blocking findings. On 2026-11-10, the local branch history was repaired without changing its tested source trees:

| Stage | Original checkpoint | Reconstructed checkpoint |
| --- | --- | --- |
| Upstream plus fork changes | `e71fc27` | `52615c3` (two parents) |
| Portal port | `1676931` | `030c7f9` |
| Original report and deployment provenance | `6f557fe` | `15bc7ae` |

Each pair has an identical Git tree. The full tree shared by `6f557fe` and `15bc7ae` is `e0e987b94bc810341ab240c4df26f2347f3f64ae`. The original candidate remains reachable through local tag `integration/upstream-20261110-reviewed-snapshot`. The only subsequent content changes are updates to this report.

After reconstruction, the locked workspace build, all-target native harnesses (489 distinct cases, plus 28 duplicated demo cases), four offscreen EGL cases and formatting checks passed again. Evidence is in `ancestry-*.log`, `ancestry-native-results.json` and `ancestry-repair.json` in the local evidence directory. No new checkout, main/PR update, remote publication or live installation was made.

## Live-validation preparation and approval boundary

Read-only host inspection confirmed the running compositor PID 8371 and portal PID 769844 still have the installed hashes below. Session target, portal frontend, GTK backend, ShojiWM backend and FD watcher were active at inspection. Recheck all state immediately before any eventual deployment.

The current config repository and actual runtime configuration are not identical: `~/.config/shojiwm/index.tsx` and `window-manager.ts` differ, and live-only drag/drop, workspace-drum and shader files exist. Both baseline and candidate static checks of the actual live tree now pass; no user config file or SDK link was changed. TypeScript 5.9.3 checked all 10 live TS/TSX roots; the candidate resolution included 36 candidate SDK files and zero installed SDK files. Logs and command details are in `live-config-check/REPORT.md` under the evidence directory.

A behavioral caveat remains: live `index.tsx` forces server decorations and does not set the newly available `kdeDefaultMode` hook. Candidate documentation warns that GTK3/KDE-default clients or clients drawing their own titlebars can behave differently, including possible double titlebars. Typechecking cannot validate this. Include GTK/KDE/client-decorated application titlebars and the live-only drag/workspace/fullscreen interactions in later visual checks; no config behavior was silently changed here.

The compositor must be deployed and rolled back with these matching components:

- `/usr/bin/shoji_wm`
- `/usr/lib/shojiwm/packages/shoji_wm/`
- `/usr/lib/shojiwm/tools/decoration-runtime.ts`

Installed SDK/runtime match the old compositor source. Do not hot-reload the candidate runtime into that old compositor. The embedded satellite is part of the compositor build and needs no separate executable install. Preserve user configuration and its existing SDK symlink.

The portal binary is independently replaceable. Its installed service, D-Bus activation file and portal descriptor already match the candidate, so those definitions need no changes. With explicit authorization and no active sharing, a staged atomic binary replacement and restart of only `xdg-desktop-portal-shojiwm.service` can validate the new portal on the old compositor. This would not establish full new-compositor compatibility.

For the full compositor upgrade, agree on a user-controlled logout/login window and install the matching binary/SDK/runtime set while the old session is stopped. Then verify actual running hashes, repeated screen-sharing cancellation and successful frames from a synthetic window, stable FD/mapping counts, NumLock, lock/unlock and mixed-DPI/XWayland popup positions.

Before replacing anything, make fresh verified backups of the current compositor, portal and entire `/usr/lib/shojiwm`. Existing historical binary backups are older than the current installation and are not sufficient rollback points for this upgrade. The portal can be rolled back independently with an idle-backend restart; compositor and SDK/runtime must be restored together for a fresh session.

Do not invoke `dist/install.sh`, `install-portal.sh`, or an install `--no-build` shortcut blindly. They can overwrite installed files, remove the runtime directory and/or restart the portal frontend more broadly than this staged plan. At preparation start the release portal was still the old installed binary and no release compositor existed in this checkout; newly built release hashes must be verified before any installation.

This report does not authorize publication, main merge, installation, session interruption, screen-sharing tests or locking the desktop. Those remain explicit next decisions. Local release preparation and static checks do not change the running session.

## Prepared release artifacts (not installed)

Locked optimized build completed successfully in 5m57s on 2026-11-10 at 19:36 JST:

`nice -n 10 cargo build --locked --release -p shoji_wm -p xdg-desktop-portal-shojiwm -j 4`

The build used source commit `15bc7ae0f07d2ff03ee6a6fa9169fce977391e22`, identical to the reviewed candidate; later edits only update this report. Both files were verified as x86-64 ELF executables. Neither was launched or installed.

| Artifact | SHA-256 |
| --- | --- |
| `target/release/shoji_wm` | `db54115c4d4d1ee34dbfc94c904a378dda20bc1062dc25f883ba344ec5efdf76` |
| `target/release/xdg-desktop-portal-shojiwm` | `b948936d8e8e89bd8d4181f070f7142bcb9c39cc4280e052fa193fb880a375dd` |

Logs: `ancestry-release-build.log`, exit marker `0`, and `ancestry-release-hashes.txt` in the evidence directory. Installed binary hashes and protected main/PR refs were rechecked after the build and remain unchanged. Recheck artifact hashes immediately before any later installation because normal Cargo builds can replace target files.

## Reproduction and evidence

- Native compile: `cargo test --locked --workspace --all-targets --no-run --message-format=json -j 4`.
- Compiled harnesses were run one at a time through `tests/run-native-tests.py`, with private runtime/config/state/cache paths and no desktop DBus/Wayland connection. Software EGL opt-in tests used the same isolation.
- Satellite: `cargo test --offline --locked --lib -j 2 -- --test-threads 1`; compile integration harness, then run each of its 45 listed cases in a fresh private runtime and process group.
- Detailed logs, runner scripts, the original pre-edit checklist and synthetic screenshots are retained locally under `/tmp/shoji-upstream-integration-20261110/`. This is temporary local evidence, not an external share link.
- Installed portal remains SHA-256 `e41956bdf07c3dc687e5d4adaaeafec42e7c7b3df1e6ca95f0029f638916cb69`; installed compositor remains `6cbe48afa897195037ccbbd1fbf6da6e385854d26d11e123b527fda2e5803f10`.
