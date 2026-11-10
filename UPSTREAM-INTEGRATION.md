# ShojiWM upstream integration candidate

**Current status, 2026-11-10 (JST):** The tested integration stack was installed at 21:02. After the user's logout/reboot, running compositor and portal hashes matched the installed candidate. PR #7 was separately merged into fork main at `22e2833612e402fb9e01612c313cd7da6cfe0e84` at 21:42. This branch now includes that main commit without changing the tested code. This publication step does not install or restart anything. See `DESKTOP-BUILD.md` for exact build provenance, installed hashes and rollback records.

Branch: `integration/upstream-20261110`.

## Scope and checkpoints

- Original fork base: `e6d5b187f137e5a256a6d4ce984faedbe22d8f71`.
- Current PR base: fork main `22e2833612e402fb9e01612c313cd7da6cfe0e84`, including the original [PR #7](https://github.com/gentianster/ShojiWM/pull/7).
- Pinned upstream baseline: `2ae3e7c9a412c4656a3908fedc46637d8ec79c79` ([source](https://github.com/bea4dev/ShojiWM/commit/2ae3e7c9a412c4656a3908fedc46637d8ec79c79)). This is the tested and deployed baseline, **not latest upstream**. At publication preparation, upstream main was `1259ac0b9a23f485845e8f820129b4612d6befaa`, with four later commits (default-theme changes, scaled-window shadow handling and formatting). Those are deliberately excluded from this PR.
- Original common ancestor: `98f84845ebbaabf0cd847d7dc5ac628e5adfccce`.
- Stage 1: two-parent merge `52615c3277d2faca82f9313432d4e22e77507bcd`, fork `e6d5b18` plus upstream `2ae3e7c`. Its tree is identical to originally verified snapshot `e71fc2753dca014b8cfc483afd46b3a31e8c4fc5`.
- Stage 2: `030c7f9246b0fb9e4238ca1a0c74baff337facb1`, the exact tree previously verified at `167693183a041edcf94becebeb8c4d7d515977b0`. It adapts PR #7 implementation `8ca5abe9672e66edf313d432ed03407bea470c6e` to the upstream lifecycle and dynamic-source model.
- Main reconciliation: merge `8c8d3da8e94d6a3a874e5cb8909c1fa3fd2121bf` has parents `4315188` and `22e2833`. Main added only original PR #7; the five overlapping portal files were already adapted in stage 2. The **entire** merge tree equals `4315188` (`5d129732e1ba4db31c88d7461ab40e63069f6f6a`), not just the conflict files. Both current main and the pinned upstream commit are ancestors.
- The original PR branch and local deployment record remain at `e452e6e0bb382c1fd28de1f936f8fc88c1bfc4cc`; no extra worktree was created. The integration PR remains a separate review and merge decision.

## Stage 1: conflict resolutions

At the original stage-one fork/upstream baselines, the fork had 16 unique commits and upstream had 71 since the common ancestor. Five files each had one textual conflict:

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

This checklist was recorded before source changes and then updated from completed commands. These are the historical stage-one/stage-two checks; the publication rerun is recorded separately below. No retry-until-green was used. Temporary logs from the earlier phase were lost during the subsequent OS reboot, so they are not claimed to remain available.

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
| Live sharing/hardware | Matching stack is installed and running hashes are verified. Actual Chrome shared frames, hardware mixed-DPI behavior, fresh-session NumLock and PAM lock/unlock remain unverified for this candidate. |

## Publication verification after PR #7 merge

Fresh checks on the code-identical `8c8d3da` tree passed on 2026-11-10. The final documentation update changes only this report and `DESKTOP-BUILD.md`.

- Locked workspace build and all-target test compilation: passed.
- All 13 compiled test harnesses ran in private IPC/config/state/cache namespaces, without connecting to the live desktop: **489 distinct tests passed**, plus 28 duplicated picker/demo cases. Four normally ignored GPU tests passed separately using surfaceless software EGL. No failures or retries.
- Portal subset: 35 cases passed, including picker, request lifecycle and resource cleanup.
- Full-workspace formatting and whitespace checks: passed. Strict Clippy for the portal binary and picker demo: passed.
- SDK TypeScript check, eight overlay cases plus fixtures, and four output-power cases: passed.
- Documentation TypeScript check and English/Japanese production builds: passed.
- Read-only code review found no materially lost PR #7 feature or new blocking regression. Both current main `22e2833` and pinned upstream `2ae3e7c` are ancestors; the main-reconciliation merge preserved the full prior candidate tree.
- Prior exact-pin satellite tests, configuration checks, synthetic GUI inspection and optimized release build were not rerun because their source and dependencies are unchanged. Their historical results and limits are listed above. Nix evaluation remains unverified after the earlier daemon-access denial; it was not retried through another route.

Fresh logs and exit codes are under `target/integration-verification-20261110/`, including `native-compile.*`, `native-results.json`, individual harness logs, `offscreen-egl.log`, `workspace-build.log`, `format.log`, `portal-clippy.log` and `web/`. These are local development artifacts, not an external download link. Remote CI status must be checked for the published head rather than inferred from these local results.

## Review, remaining edges and next decision

Final read-only review found no confirmed new blocking regression in the stage-two delta. The limitations below are retained explicitly rather than treated as validated behavior.

The thumbnail worker can interrupt an established Wayland connection. The initial `Connection::connect_to_env()` call occurs before socket registration and is not made interruptible by this patch. Thus a pathological stall during initial connection setup is not covered by the blocked-roundtrip test. Aborting the outer `SelectSources` future also drops its joining guard inline instead of reaching normal `spawn_blocking` teardown; this pre-existing path can still block during that setup edge. Tests cover picker-future abort and thumbnail interruption separately, not an end-to-end outer-future abort. Cancellation during actual asynchronous window opening has state-level coverage only, not a real UI event-order test.

The local integration history records both pinned upstream and merged PR #7 ancestry. The user requested continuation to publish the integration for review. Merging this integration into main and any further deployment remain separate decisions. Live validation should explicitly cover repeated picker open/cancel/share/stop, successful browser frame delivery, stable thumbnail FD/mapping counts after cancellation, NumLock, lock/unlock, and hardware mixed-DPI/XWayland popup positions. Preserve the existing installed-binary rollback plan.

## Local history repair after review

Muse reviewed the original candidate read-only and reported no blocking findings. On 2026-11-10, the local branch history was repaired without changing its tested source trees:

| Stage | Original checkpoint | Reconstructed checkpoint |
| --- | --- | --- |
| Upstream plus fork changes | `e71fc27` | `52615c3` (two parents) |
| Portal port | `1676931` | `030c7f9` |
| Original report and deployment provenance | `6f557fe` | `15bc7ae` |

Each pair has an identical Git tree. The full tree shared by `6f557fe` and `15bc7ae` is `e0e987b94bc810341ab240c4df26f2347f3f64ae`. The original candidate remains reachable through local tag `integration/upstream-20261110-reviewed-snapshot`. Subsequent content changes before main reconciliation are deployment and verification updates in this report and `DESKTOP-BUILD.md`; source code is unchanged.

After reconstruction, the locked workspace build, all-target native harnesses (489 distinct cases, plus 28 duplicated demo cases), four offscreen EGL cases and formatting checks passed again. Those earlier logs were temporary and did not survive the later OS reboot. No new checkout, main/PR update, remote publication or live installation was made during that reconstruction phase.

## Current deployment and live-verification boundaries

The user approved the full stack update and chose atomic replacement before their own logout/login. Both executables and the matching SDK/runtime were installed at 21:02 JST with verified backups. After logout/reboot, the actual compositor and portal executables matched the candidate hashes below. Runtime files and metadata also match the staged manifest. No configuration files or SDK symlinks were changed. Publication preparation performs no further installation, service restart, config reload, logout or lock test.

The user reported non-responsive mouse movement/buttons after the update, persisting through their reboot, then reported recovery during read-only diagnosis. No input settings, device resets, injected events, reloads or rollback were performed during that diagnosis. The cause and recovery mechanism are **not established**, and this is not claimed as a code fix. Both new executables remained active when recovery was reported.

The configuration repository and actual runtime configuration are not identical. The live tree is `~/.config/shojiwm/src/`; its `index.tsx` and `window-manager.ts` differ from the configuration repository, and live-only drag/drop, workspace-drum and shader files exist. Earlier baseline and candidate static checks of all 10 live TS/TSX roots passed with TypeScript 5.9.3; candidate resolution included 36 candidate SDK files and zero installed SDK files. No config or SDK-link changes were made. Those temporary logs were lost on reboot, and this publication step does not repeat live-config checks because the code and SDK are unchanged.

A behavioral caveat remains: live `index.tsx` forces server decorations and does not set the new `kdeDefaultMode` hook. GTK3/KDE-default clients or clients drawing their own titlebars may behave differently, including possible double titlebars. Typechecking does not verify this. Later authorized visual checks should include those titlebars, live-only drag/workspace/fullscreen interactions, successful browser frame delivery, repeated picker open/cancel/share/stop, stable FD/mapping counts, NumLock, lock/unlock and hardware mixed-DPI/XWayland popup positions.

The compositor and SDK/runtime must be deployed or rolled back as a matching set: `/usr/bin/shoji_wm`, `/usr/lib/shojiwm/packages/shoji_wm/` and `/usr/lib/shojiwm/tools/decoration-runtime.ts`. The embedded satellite is part of the compositor build. The portal binary can be changed separately only with its own compatibility and idle-sharing checks. Service definitions, D-Bus activation and the portal descriptor did not need changes.

The complete old stack is backed up under `/usr/local/lib/shojiwm-backups/upstream-20261110-13ed0a9/old/`. Preserve this rollback set and the user configuration. Do not blindly invoke install scripts: they can replace the runtime or restart more services than intended. A future publication or merge does not authorize a new installation or session interruption.

## Release artifacts and deployment provenance

Locked optimized build completed successfully in 5m57s on 2026-11-10 at 19:36 JST:

`nice -n 10 cargo build --locked --release -p shoji_wm -p xdg-desktop-portal-shojiwm -j 4`

The build used source commit `15bc7ae0f07d2ff03ee6a6fa9169fce977391e22`, with the exact tree of reviewed candidate `6f557fe`. Later checkpoint `13ed0a9` was recorded in the deployment manifest and differs only in documentation. Both files were verified as x86-64 ELF executables, installed at 21:02 JST, and activated by the user's subsequent session restart. Main reconciliation and publication documentation do not alter their source code.

| Artifact | SHA-256 |
| --- | --- |
| `target/release/shoji_wm` | `db54115c4d4d1ee34dbfc94c904a378dda20bc1062dc25f883ba344ec5efdf76` |
| `target/release/xdg-desktop-portal-shojiwm` | `b948936d8e8e89bd8d4181f070f7142bcb9c39cc4280e052fa193fb880a375dd` |

The original temporary release-build logs were lost on reboot. Durable root-owned deployment evidence remains in `result.json`, `manifest.json` and `apply-now.log` under `/usr/local/lib/shojiwm-backups/upstream-20261110-13ed0a9/`; candidate and old-stack copies are retained there. Installed and running hashes were verified after the user restarted the session and again after the PR #7 merge. Recheck artifacts before any later installation because normal Cargo builds can replace target files.

## Reproduction and evidence

- Native compile: `cargo test --locked --workspace --all-targets --no-run --message-format=json -j 4`.
- Compiled harnesses were run one at a time through `tests/run-native-tests.py`, with private runtime/config/state/cache paths and no desktop DBus/Wayland connection. Software EGL opt-in tests used the same isolation.
- Satellite: `cargo test --offline --locked --lib -j 2 -- --test-threads 1`; compile integration harness, then run each of its 45 listed cases in a fresh private runtime and process group.
- Earlier logs and synthetic screenshots under `/tmp/shoji-upstream-integration-20261110/` did not survive the reboot. They are historical results, not downloadable artifacts.
- Fresh publication verification logs are saved under the ignored, non-temporary `target/integration-verification-20261110/` in this checkout. Build outputs remain regenerable and are not source backups.
- Current installed and running hashes are the release-artifact hashes above; the previous hashes and full rollback set are recorded in `DESKTOP-BUILD.md`.
