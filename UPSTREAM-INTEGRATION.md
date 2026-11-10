# ShojiWM upstream integration candidate

Date: 2026-11-10 (JST). Branch: `integration/upstream-20261110`.

## Scope and checkpoints

- Fork base: `e6d5b187f137e5a256a6d4ce984faedbe22d8f71` (`origin/main`).
- Upstream baseline: `2ae3e7c9a412c4656a3908fedc46637d8ec79c79` ([source](https://github.com/bea4dev/ShojiWM/commit/2ae3e7c9a412c4656a3908fedc46637d8ec79c79)).
- Common ancestor: `98f84845ebbaabf0cd847d7dc5ac628e5adfccce`.
- Stage 1: `e71fc2753dca014b8cfc483afd46b3a31e8c4fc5`, upstream plus fork-specific changes, verified before porting PR #7.
- Stage 2: the following portal changes adapt [PR #7](https://github.com/gentianster/ShojiWM/pull/7), implementation `8ca5abe9672e66edf313d432ed03407bea470c6e`, to that verified baseline.
- Stage 1 is a normal local snapshot commit, **not a two-parent upstream merge**. Main and PR refs are unchanged. Final upstream parentage remains to be arranged when an actual merge is authorized.
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
| Personal config | Candidate-SDK and existing-SDK typechecks passed. 67 mocked cases passed: 25 workspace, 20 switcher, 16 overview and 6 QML transitions. Existing dirty files, status and SDK symlink remained unchanged. |
| Documentation | Typecheck and English/Japanese production builds passed; docs locks unchanged. |
| Nix | Not verified: evaluation with invocation-only `nix-command flakes` reached the daemon but failed with Permission denied on `/nix/var/nix/daemon-socket/socket`. No permissions, config, services or lockfiles were changed. |
| Visual picker QA | Japanese synthetic windows at widths 720 and 360 rendered successfully. Selected, empty and no-match states inspected; footer actions remain visible, long titles wrap, Share is disabled without valid selection. Only each demo's own renderer was saved, never desktop/source content. |
| Live sharing/hardware | Candidate not installed. Actual Chrome shared frames, hardware mixed-DPI behavior, fresh-session NumLock and PAM lock/unlock require separately authorized live validation. |

## Review, remaining edges and next decision

Final read-only review found no confirmed new blocking regression in the stage-two delta. The limitations below are retained explicitly rather than treated as validated behavior.

The thumbnail worker can interrupt an established Wayland connection. The initial `Connection::connect_to_env()` call occurs before socket registration and is not made interruptible by this patch. Thus a pathological stall during initial connection setup is not covered by the blocked-roundtrip test. Aborting the outer `SelectSources` future also drops its joining guard inline instead of reaching normal `spawn_blocking` teardown; this pre-existing path can still block during that setup edge. Tests cover picker-future abort and thumbnail interruption separately, not an end-to-end outer-future abort. Cancellation during actual asynchronous window opening has state-level coverage only, not a real UI event-order test.

Before installation, choose the final Git integration history and complete review. Do not treat the local checkpoint as an upstream ancestry merge. Live validation should explicitly cover repeated picker open/cancel/share/stop, successful browser frame delivery, stable thumbnail FD/mapping counts after cancellation, NumLock, lock/unlock, and hardware mixed-DPI/XWayland popup positions. Preserve the existing installed-binary rollback plan.

## Reproduction and evidence

- Native compile: `cargo test --locked --workspace --all-targets --no-run --message-format=json -j 4`.
- Compiled harnesses were run one at a time through `tests/run-native-tests.py`, with private runtime/config/state/cache paths and no desktop DBus/Wayland connection. Software EGL opt-in tests used the same isolation.
- Satellite: `cargo test --offline --locked --lib -j 2 -- --test-threads 1`; compile integration harness, then run each of its 45 listed cases in a fresh private runtime and process group.
- Detailed logs, runner scripts, the original pre-edit checklist and synthetic screenshots are retained locally under `/tmp/shoji-upstream-integration-20261110/`. This is temporary local evidence, not an external share link.
- Installed portal remains SHA-256 `e41956bdf07c3dc687e5d4adaaeafec42e7c7b3df1e6ca95f0029f638916cb69`; installed compositor remains `6cbe48afa897195037ccbbd1fbf6da6e385854d26d11e123b527fda2e5803f10`.
