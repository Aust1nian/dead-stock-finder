# Test report

The state of every test layer the spec asks for (SPEC § Tests, CI and install tests), with links to the runs. "Pending" means the layer needs something only Austin can provide.

Repo: https://github.com/Aust1nian/idle-stock-finder (public; CI is free).

## Summary

| Layer | Where | Status |
|---|---|---|
| Unit tests (label rules, allowlist, PKCE, keychain, diffs, export safety, redaction, scheduling logic) | every push, ubuntu-24.04 | ✅ |
| Mock Square scenarios (22) | every push, ubuntu-24.04 | ✅ |
| Generator and property tests, 200 seeds | every push | ✅ |
| Generator and property tests, 2,000 seeds | nightly | ✅ (local run 2026-10-12; nightly from tonight) |
| Speed and memory targets | nightly (ubuntu-24.04, macos-15) | ✅ all four (local run 2026-10-12) |
| Allowlist trace (strace, Linux) | every push | ✅ zero connections outside loopback |
| Self-test with only loopback reachable | every push: macos-15, macos-26, macos-15-intel, windows-2025, windows-11-arm | ✅ all five |
| Scheduler tests (register, read back, fire, no window, lock, unregister) | every push: macos-15, macos-26, windows-2025 | ✅ all three |
| UI tests (WebDriver, full owner path) | every push: ubuntu-24.04, windows-2025 | ✅ 14 of 14 steps on both |
| Install tests, including the upgrade | release candidates (`release.yml`): macos-15, macos-26, macos-15-intel, windows-2025, windows-11-arm | ✅ all five (unsigned; signing checks skipped until the secrets exist) |
| Square Sandbox | nightly and release, once `SQUARE_SANDBOX_TOKEN` exists | **pending**: no Square developer account yet |
| Field test (real reboot, real Square screens, a week of checks) | Phase 7, by hand | **pending**: `docs/FIELD_TEST.md` |

## Mock Square scenarios (`core/check/tests/mock_scenarios.rs`)

All 22 pass, on every push. They cover each scenario the spec lists:

| Spec scenario | Test |
|---|---|
| Paging across many pages | `paging_across_many_pages`: 2,500 items and 2,500 orders over several pages |
| Rate limits with retry delays | `rate_limits_are_retried`: every third request answers 429 |
| Expired access token | `expired_access_token_is_refreshed_once_and_saved` |
| Rejected refresh token | `rejected_refresh_means_disconnected_and_one_notification`: labels untouched, the token deleted, one notification |
| Network drop mid-sync | `network_drop_mid_sync_fails_safe_and_the_next_check_recovers`; `interrupted_at_every_request_then_rerun_gives_the_same_labels` cuts the connection at every single request of a full check |
| Version conflicts on write | `version_conflict_refetches_and_keeps_the_staff_edit`; `conflict_three_times_skips_that_item_only` |
| An object that comes back changed in more than the label | `write_that_comes_back_changed_stops_all_writes`; `merged_staff_edit_during_a_write_stops_writes_fail_safe` (Square's three-way merge) |
| Deleted and archived items | `deleted_and_new_items_between_checks`, and the archived item in every scenario |
| An item with 50 variations | the "T-shirt" item in every scenario |
| Several locations in different time zones; daylight saving | New York and Los Angeles locations (a sale at 23:30 Pacific shows that day); DST is covered in `dsf-compute` unit tests |
| Orders closed or edited after the checkpoint | `late_and_edited_orders_after_the_checkpoint` (a POS sale arriving 2 days late; an order whose line changed) |
| Returns, canceled orders, custom-amount lines | in every scenario: none of them count |
| Hand-edited labels are put back | `staff_changing_a_label_by_hand_is_put_back` |
| Remove all labels returns the catalog to its starting snapshot | `remove_all_labels_restores_the_catalog_exactly` |
| A second check writes nothing | `first_check_labels_everything_and_second_writes_nothing` |
| Tokens never in any file | `tokens_never_reach_any_file` |
| 20,000 items; 100,000 orders | the nightly speed-and-memory run (below) |

## CI runs

| Run | What | Result |
|---|---|---|
| [38241270607](https://github.com/Aust1nian/idle-stock-finder/actions/runs/38241270607) | Latest code: all 14 jobs | ✅ all green |
| [38241215255](https://github.com/Aust1nian/idle-stock-finder/actions/runs/38241215255) | Release candidate 0.1.0: builds, install tests on five runners | ✅ all green |
| [38236686258](https://github.com/Aust1nian/idle-stock-finder/actions/runs/38236686258) | First release candidate | Mac ✅; Windows ❌: the test started the marked installer through the shell, whose security box can't be clicked in a batch session. Fixed by starting it directly |
| [38235262754](https://github.com/Aust1nian/idle-stock-finder/actions/runs/38235262754) | Phase 5 commit: all 14 jobs | ✅ all green |
| [38233548421](https://github.com/Aust1nian/idle-stock-finder/actions/runs/38233548421) | Phase 0: first full matrix | macOS scheduler ✅; Windows registration as a standard user ❌ (task folder needs admin; fixed by moving the task to the root); Linux keyring ❌ (fixed) |

### Scheduler evidence (run 38235262754)

- **macos-15 and macos-26.**
  - `SMAppService` registered both agents ("enabled").
  - `launchctl print` shows both with `--check` arguments, and `sfltool dumpbtm` lists them under Login Items.
  - The sign-in agent fired (`not_connected`, windows 0), the daily tick fired (`skipped: not due`), and a held lock made a third run step aside.
  - After unregistering, launchd lists neither agent.
- **windows-2025.**
  - The task "Idle Stock Finder check (runneradmin)" has a LogonTrigger, a CalendarTrigger at 05:00, `InteractiveToken`, least privilege and a 10-minute limit.
  - `schtasks /run` started a check that owned 0 windows, and the lock stopped an overlapping run.
  - A separate **standard user** registered their own task with both triggers, with no admin rights.
  - Unregistering and uninstalling left no task, no Run entry and no app.

## Install tests (release run 38241215255)

**Sizes:** Mac disk image 13 MB; Windows installer 211 MB. The installer embeds the WebView2 runtime, as the spec asks; the choice is open for Austin in DECISIONS.md, Packaging.

| Check | macos-15 | macos-26 | macos-15-intel | windows-2025 | windows-11-arm (x64 under emulation) |
|---|---|---|---|---|---|
| Marked as downloaded (quarantine / Mark of the Web) | ✅ | ✅ | ✅ | ✅ | ✅ |
| Gatekeeper / signature | skipped (unsigned) | skipped | skipped | skipped | skipped |
| No admin: Windows manifest `asInvoker`, installed by a standard user | — | — | — | ✅ | ✅ |
| `--self-test` from the installed copy | ✅ | ✅ | ✅ | ✅ | ✅ |
| Background check registered; listed under Login Items (Mac) or as the user's task (Windows) | ✅ | ✅ | ✅ | ✅ | ✅ |
| Upgrade 0.0.99 → 0.1.0 keeps the connection, the cache (byte-identical on Windows) and the background check | ✅ | ✅ | ✅ | ✅ | ✅ |
| Uninstall removes the task and the app, keeps the cache unless asked | — | — | — | ✅ | ✅ |

## Generator and property tests

`gen/` builds a seeded Square seller (catalog, locations, a year and more of orders, stock) and works out every label from Square's JSON. It never reads this app's code: a separate agent wrote it from the rules in DECISIONS.md. Each seed then runs through the real check against the mock (`core/check/tests/properties.rs`):

| Property | 200 seeds (every push) | 2,000 seeds (nightly; also run locally 2026-10-12) |
|---|---|---|
| Every label equals the generator's truth, after the first check and after each later one (0–4 later checks with new sales, edits, new items, deletes, hand-typed labels) | ✅ | ✅ (68 s, release build) |
| Returns never move a last-sold date (every seed has returns and exchanges) | ✅ | ✅ |
| A second check with no new orders writes nothing | ✅ | ✅ |
| Every write differs from the fetched object only in this app's key (mock audit) | ✅ 0 violations | ✅ 0 violations |
| Killing the check at a request chosen by the seed, then running again, ends with the same labels (every fifth seed) | ✅ | ✅ |
| No request outside the allowlist | ✅ | ✅ |
| No token in the cache, the log, an export or diagnostics | ✅ | ✅ |

What the seeds cover, by seed (gen's own tests confirm each appears):
- every window from {30, 60, 90, 120, 180, 365, 400}, so the history depth is 12, 14 or 15 months;
- sales exactly at the window and one day past it;
- late-night sales whose local date differs from UTC;
- daylight-saving changes;
- one to four locations in different zones, with unselected and inactive locations;
- canceled and open orders; custom-amount and gift-card lines; zero, negative and decimal quantities;
- returns and exchanges;
- orders edited after the checkpoint, and late-arriving orders;
- new items and variations, and variations removed;
- archived and deleted items;
- the seller's own `Square:` attributes;
- stale and hand-typed labels;
- items with up to 50 variations.

## Speed and memory

`dsf-perf run --items 20000 --orders 100000 --latency-ms 300 --mbps 20` (`bench/`):
- The mock Square runs in its own process, behind a simulated office connection: 300 ms per request, 20 Mbit/s, and orders padded to 2.5 KB to stand in for real Square orders' taxes and tenders.
- Each check runs in its own process, so the memory measured is the check's alone.
- 20,000 items with 39,926 variations; 100,000 generated orders, 63,357 of them completed within the 12-month read; 3 locations.

Measured 2026-10-12 on the development machine (Linux), after the speed fixes below. The same run is nightly on ubuntu-24.04 and macos-15.

| Target (SPEC, Hard constraints 6) | Measured | |
|---|---|---|
| First sync, 12 months and up to 100,000 orders, under 10 minutes | **118 s**: catalog 16 s, orders 92 s, stock counts 9 s | ✅ |
| Daily check with up to 2,000 new orders, under 60 s | **24 s**: orders 3 s, 1,877 labels written in 12 s, catalog read-back of its own writes 7 s | ✅ |
| Peak memory at or under 300 MB with 20,000 items | **77 MB** (the first sync alone: 25 MB) | ✅ |
| Every write changed only the label | **0 violations** in 21,306 audited writes | ✅ |
| (Not a target) First labeling of every item | 149 s for 19,429 items | |

How it got there. The first measurement missed the daily target at **112 s**: 58 s of it re-read the catalog, because a one-day overlap covered the previous check's own writes. That was fixed by:
- starting each catalog read exactly at Square's `latest_time`;
- writing in batches of 1,000 objects;
- fetching six calls at a time.

The reasoning is in DECISIONS.md (2026-10-12, "The catalog read begins exactly at Square's latest_time"). The real network figures depend on Square's actual response times, which the Phase 7 field test records.

## Known failures

None open in CI.

## Field test

Pending (`docs/FIELD_TEST.md`).
