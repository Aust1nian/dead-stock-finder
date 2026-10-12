# Test report

The state of every test layer the spec asks for (SPEC § Tests, CI and install tests), with links to the runs. "Pending" means the layer needs something only Austin can provide.

Repo: https://github.com/Aust1nian/dead-stock-finder (public; CI is free).

## Summary

| Layer | Where | Status |
|---|---|---|
| Unit tests (label rules, allowlist, PKCE, keychain, diffs, export safety, redaction, scheduling logic) | every push, ubuntu-24.04 | ✅ |
| Mock Square scenarios (22) | every push, ubuntu-24.04 | ✅ |
| Generator and property tests, 200 seeds | every push | see "Generator and property tests" |
| Generator and property tests, 2,000 seeds | nightly | see "Generator and property tests" |
| Speed and memory targets | nightly (ubuntu-24.04, macos-15) | see "Speed and memory" |
| Allowlist trace (strace, Linux) | every push | ✅ zero connections outside loopback |
| Self-test with only loopback reachable | every push: macos-15, macos-26, macos-15-intel, windows-2025, windows-11-arm | ✅ all five |
| Scheduler tests (register, read back, fire, no window, lock, unregister) | every push: macos-15, macos-26, windows-2025 | ✅ all three |
| UI tests (WebDriver, full owner path) | every push: ubuntu-24.04, windows-2025 | ✅ 14 of 14 steps on both |
| Install tests, including the upgrade | release candidates (`release.yml`) | first run at the first release candidate |
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
| [38235262754](https://github.com/Aust1nian/dead-stock-finder/actions/runs/38235262754) | Phase 5 commit: all 14 jobs | ✅ all green |
| [38233548421](https://github.com/Aust1nian/dead-stock-finder/actions/runs/38233548421) | Phase 0: first full matrix | macOS scheduler ✅; Windows registration as a standard user ❌ (task folder needs admin; fixed by moving the task to the root); Linux keyring ❌ (fixed) |

### Scheduler evidence (run 38235262754)

- **macos-15 and macos-26.**
  - `SMAppService` registered both agents ("enabled").
  - `launchctl print` shows both with `--check` arguments, and `sfltool dumpbtm` lists them under Login Items.
  - The sign-in agent fired (`not_connected`, windows 0), the daily tick fired (`skipped: not due`), and a held lock made a third run step aside.
  - After unregistering, launchd lists neither agent.
- **windows-2025.**
  - The task "Dead Stock Finder check (runneradmin)" has a LogonTrigger, a CalendarTrigger at 05:00, `InteractiveToken`, least privilege and a 10-minute limit.
  - `schtasks /run` started a check that owned 0 windows, and the lock stopped an overlapping run.
  - A separate **standard user** registered their own task with both triggers, with no admin rights.
  - Unregistering and uninstalling left no task, no Run entry and no app.

## Generator and property tests

To be filled in by the first run with the generator (`core/check/tests/properties.rs`).

## Speed and memory

To be filled in by the first nightly run (`bench/`, `dsf-perf run`). It uses a simulated office connection of 300 ms per request and 20 Mbit/s.

## Known failures

None open in CI.

## Field test

Pending (`docs/FIELD_TEST.md`).
