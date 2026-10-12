# Dead Stock Finder for Square — Build Spec

Oct 9, 2026 · @Austin Burnes

A free desktop app for Square sellers that finds items that haven't sold in a window the owner picks, and writes a "last sold" label onto each item inside Square, so staff see it without opening the app. It connects with the owner's own Square login, keeps everything on their computer, uses no AI model, and refreshes itself in the background every day and at every sign-in.

## How to run this build

Build with Claude Code on Opus 5.5 at the highest reasoning effort, in a fresh repo with this file saved as `SPEC.md`.

- Read the whole spec before writing code. Work phase by phase (see Build phases); a phase is done only when its gate passes.
- Log every judgment call in `DECISIONS.md`: date, the choice, the reason, what it rules out. Open decisions have working defaults: build on the default and log it, don't stop to ask.
- Never change a test's expected output to make it pass. If a fixture's truth is wrong, log why and fix it in its own commit.
- Verify every Square fact in this spec at Phase 0 against Square's current docs and a Sandbox account. Where reality differs, log it and adapt.
- **Never risk a seller's catalog.** Every write to Square changes only this app's own label field, and a test proves it on every write path (see Labels in Square).
- The app talks to Square's API and nothing else. No telemetry, no update checks, no other hosts, proven by test.
- Stop and ask Austin only for open decisions, signing or Square credentials, or anything that costs money.

## Open decisions for Austin

Seven calls are Austin's. The build uses these defaults until he changes them and logs which it used.

| # | Decision | Working default |
| --- | --- | --- |
| 1 | App name and branding | Working title "Dead Stock Finder"; company name on the window, the download page, and Square's approval page |
| 2 | Who owns the Square developer app | The company's Square developer account. Its name is what sellers see on Square's approval screen |
| 3 | Default slow window | 90 days; the owner can set any number of days |
| 4 | Label wording | `Last sold Jul 14, 2026` inside the window; `SLOW · last sold Jul 14, 2026` past it; `SLOW · no sales in 12+ months` when the whole history read has no sale; `No sales since added Aug 2, 2026` for newer items. Dates, not day counts, so a label is never wrong while the computer is off |
| 5 | Labels on by default | The setup screen asks, with the switch pre-set to on and one plain sentence on what it writes |
| 6 | Daily check time | 5:00 a.m. local, plus every sign-in (see The headless check) |
| 7 | Also list the Windows build in the Microsoft Store | No for v1: direct download plus the salesperson's recovery card |

## What it does

It answers one question every day: which items haven't sold in N days? It answers in two places, the app's own report and a label on each item inside Square.

- **Tracks every item and variation automatically.** It reads the seller's item list and sales from Square, and keeps the date each item and variation last sold.
- **The owner sets the window.** 30, 60, 90 days or any number, and which locations count.
- **The report** lists every slow item with its last-sold date, days since, units on hand, and the value tied up (on hand × price, and × unit cost where Square has one). It can be sorted, filtered and exported.
- **The label** is written onto each item in Square as a custom attribute, so staff see it in Square's own screens.
- **It stays current on its own.** A headless check runs once a day and at every sign-in after a boot or restart.

**For:** retail sellers on Square who carry enough items that slow sellers hide: boutiques, gift shops, hardware and parts counters, bottle shops. **Not for:** sellers with a handful of items, or sellers whose sales don't go through Square.

It does not change prices, reorder stock, mark items down, or touch anything in Square except its own label.

## Hard constraints

Each of these is tested; a build that breaks one does not ship.

1. **Talks only to Square.** The only hosts the app may reach are `connect.squareup.com` and, in development builds only, `connect.squareupsandbox.com`. An allowlist in code enforces it; a test proves it.
2. **Never damages the catalog.** Every write changes only this app's own label attribute. If the app can't prove a write is label-only, it skips that item and logs why.
3. **No data leaves the computer** except API calls to Square. The token lives in the operating system's keychain; the sales cache lives in the per-user app data folder.
4. **No admin rights,** for install or for background scheduling.
5. **Platforms.** macOS 11 or later as a universal app; Windows 10 22H2 and Windows 11, x64. Linux is not shipped.
6. **Light.** Peak memory at or under 300 MB with 20,000 items. A daily check with up to 2,000 new orders finishes in under 60 seconds; the first sync of 12 months and up to 100,000 orders finishes in under 10 minutes on a normal office connection.
7. **Headless means silent.** Background checks open no window and steal no focus. The only thing they ever show is a system notification when the owner must act.
8. **Fails safe.** If Square can't be reached or the connection has lapsed, labels stay as they are (their dates keep them true) and the next check tries again.
9. **Small download.** No model, so the installer should be tens of megabytes; the release workflow prints the real size.

## What the owner sees

The owner opens the window twice: once to connect and set up, and later only if they want the full report or a change.

1. **First open.** One sentence on what it does and one button, **Connect Square**. Under it in small type: "Reads your items and sales. Writes one 'Last sold' label on your items. Nothing leaves this computer except requests to Square."
2. **Square's own page** opens in their browser. They log in and approve. The app comes back to the front: "Connected to Main St Boutique."
3. **One setup screen.** Slow window (90 days), which locations count, **Labels in Square** (on), daily check time (5:00 a.m.), **Check in the background** (on). One button: **Start**.
4. **First sync.** A progress bar ("Reading sales: 4 of 12 months"), then the report.
5. **The report.** One table: item, variation, category, last sold, days since, on hand, value tied up, label status. Filters for window, location, category and slow-only. **Save** writes XLSX or CSV through the system dialog.
6. **Status line.** "Connected to Main St Boutique · last check today 5:02 a.m. · next tomorrow 5:00 a.m. · labels on 214 items," with **Check now**.
7. **Settings.** Everything from setup, plus **Remove all labels from Square** and **Disconnect Square**. Disconnect deletes the token from the computer and tells the owner they can also remove the app in Square Dashboard under Settings, then App integrations.
8. **Notifications, only when the owner must act:** "Reconnect Square to keep labels current," or "Checks have failed 3 days in a row," with the reason.

Nothing stays running between checks: no tray icon, no menu bar icon. The background check is launched by the operating system and exits when done.

## Connecting to Square

The app uses Square's OAuth PKCE flow, which Square requires for native desktop apps because they can't safely store a client secret. No secret ships in the app, and the seller logs in only on Square's own page. ([Square OAuth](https://developer.squareup.com/docs/oauth-api/overview))

**One-time setup (Austin):** create a production application in the company's Square Developer Console, register the redirect URL `http://localhost:<port>`, and give Claude Code the application ID. The `<port>` placeholder is Square's supported way for PKCE desktop clients to use a port picked at runtime. ([Square: redirect URLs](https://developer.squareup.com/docs/oauth-api/create-urls-for-square-authorization))

**The connect flow:**

1. Generate a code verifier, its SHA-256 challenge, and a random state value.
2. Start a listener on `127.0.0.1` on a free port, for one request, with a 5-minute timeout.
3. Open the browser to Square's authorize page with the app ID, scopes, state, challenge, the redirect URL with that port, and `session=false`, so the seller always logs in to the right account.
4. On the callback, check the state, then exchange the code and the verifier for tokens.
5. Save the tokens to the keychain before doing anything else.

**Permissions requested:** `MERCHANT_PROFILE_READ` (business name, locations), `ITEMS_READ`, `ORDERS_READ`, `INVENTORY_READ`, and `ITEMS_WRITE` only when labels are on. Turning labels on later reconnects with the extra permission. Phase 0 confirms each name against Square's current list.

**Tokens.** Access tokens expire after 30 days. Refresh tokens from the PKCE flow work once and expire after 90 days. So:

- Every check refreshes the access token if it is more than 7 days old. Each refresh returns a new refresh token, which is saved to the keychain immediately.
- As long as a check runs at least once every 80 days, the connection never lapses.
- If a refresh fails, the app marks itself disconnected and notifies the owner to reconnect.

**Storage.** macOS Keychain and Windows Credential Manager, per user. Tokens never appear in files, logs, exports or diagnostics.

**Phase 0 must confirm two things in production:** that the `localhost` redirect works for PKCE (Square requires HTTPS for production redirects in general), and that custom URL schemes are indeed unsupported. If localhost fails, the fallback is a static HTTPS page on the company domain that forwards the code to the local listener. The code alone is useless without the verifier, which never leaves the computer.

## Sales sync and the last-sold calculation

The app keeps a local copy of what it needs from Square, updates it a little each check, and recomputes every label from that copy. All of it is plain code.

**What it reads:**

- **Locations,** with each location's time zone. Every date is shown in the seller's local time.
- **The catalog:** every item and variation with its name, category, price, unit cost where Square has one, and which locations carry it. Deleted and archived items are skipped. The app records the date it first saw each variation.
- **Orders:** completed orders at the selected locations. The first sync reads 12 months back, or the slow window plus 30 days if that is longer. Every later check reads orders updated since the last checkpoint, minus a one-day overlap to catch orders closed or edited late, with duplicates removed by order ID and version.
- **Inventory counts** (in stock, per location) for on hand and value tied up. Items that don't track inventory show a blank.

**What counts as a sale:** a line item on a completed order that points to a catalog variation, with a quantity above zero. Returns never reset or reverse a last-sold date. Canceled orders and custom-amount lines without a catalog item are ignored.

**The calculation:**

- A variation's last-sold date is its latest sale at the selected locations. An item's is the latest across its variations.
- Slow means the last sale is older than the window.
- An item with no sale anywhere in the history read is "no sales in 12+ months" (or however far back the read went), never a made-up date. An item added after the first sync with no sales yet is "no sales since added," with its first-seen date, and only becomes slow once it has existed longer than the window. If Phase 0 finds Square provides a creation date for catalog objects, use that instead of first-seen.
- **Labels are account-wide, not per location.** Square sets custom attribute values at a global, location-independent level, so the label reflects all selected locations combined. Per-location detail lives in the report. ([Square: CatalogObject](https://developer.squareup.com/reference/square/objects/CatalogObject))

**The local copy** is a SQLite file in the per-user app data folder, holding the catalog snapshot, sale dates per variation, the order checkpoint and the labels last written. Recomputing everything from it takes well under a second.

## Labels in Square

The label is a Square catalog custom attribute that the app creates once and then updates only when its text changes. Because labels are dates, a label changes only when the item sells again or crosses the window, so a typical day writes few items.

**The attribute.** One definition, created on first use: type string, name "Last sold," allowed on items and variations, seller visibility `SELLER_VISIBILITY_READ_WRITE_VALUES`, so sellers can see it in Square's screens. Its description, which Square may show as a tooltip, reads "Updated daily by Dead Stock Finder." If staff edit a value by hand, the next check puts it back. ([Square: seller visibility](https://developer.squareup.com/reference/square/enums/CatalogCustomAttributeDefinitionSellerVisibility))

**The safe-write procedure (every write, no exceptions):**

1. Fetch the current object fresh from Square.
2. Copy it and change only this app's key in its custom attribute values.
3. Diff the outgoing object against the fetched one. The only allowed difference is that key; anything else and the write is skipped and logged.
4. Send it with its version number. Square rejects a write whose version doesn't match, so an edit by staff in between can't be overwritten. On a conflict, start again from step 1, up to 3 times, then skip and log. ([Square: CatalogObject](https://developer.squareup.com/reference/square/objects/CatalogObject))
5. Read it back and diff again. If anything besides the label changed, stop all writes, notify the owner, and log the full before-and-after.

Writes go in batches, one batch at a time, with backoff when Square returns a rate-limit error.

**Remove all labels** clears every value this app wrote, using the same procedure, then deletes the definition.

**Phase 0 must settle four things on a Sandbox account and record the answers in `DECISIONS.md`:**

- Exactly what a catalog write must include so nothing else changes, for example whether leaving out an item's variations removes them. The safe-write procedure is built on that answer.
- That values persist. A Square developer forum thread reports values silently not saving in Sandbox. ([Square forum](https://developer.squareup.com/forums/t/custom-attribute-values-on-an-item-silently-dont-persist-sandbox/27075))
- Where the label appears in the Dashboard and the point-of-sale app, with screenshots, and whether sellers can search or filter by it.
- Whether item-level, variation-level or both labels show. Write only the levels that show.

If labels can't be written reliably, the app ships report-only and the build stops to tell Austin.

## The headless check

The operating system launches the app with `--check`, it updates everything with no window, and it exits. Three triggers launch it:

1. **Daily** at the chosen time (default 5:00 a.m.) while the computer is on.
2. **At every sign-in,** which covers every boot and restart.
3. **On wake,** if the computer was asleep at the daily time.

&#91;embedded content: headless check · 3 triggers, 7 steps, 3 exits\]

Every trigger runs the same check. Only step 6 writes to Square, and it stops everything if a write changed more than the label.

The Check now button runs the same code with a window.

**Sign-in, not power-on.** The check runs when the owner signs in after a boot or restart. Running before anyone signs in would need an admin-installed system service, which breaks the no-admin rule and couldn't reach the owner's keychain anyway. A computer that sits at the sign-in screen gets checked once someone signs in.

**On Mac:** a LaunchAgent bundled inside the app, with `RunAtLoad` (runs at sign-in) and `StartCalendarInterval` (the daily time; a time missed while asleep runs on wake). On macOS 13 and later it is registered with `SMAppService`, so macOS lists it under Login Items, where the owner can switch it off. On macOS 11–12 the same plist goes in `~/Library/LaunchAgents` and is loaded with `launchctl`.

**On Windows:** a per-user Task Scheduler task created by the app as the signed-in user, with no admin. Triggers: at log on of this user, and daily at the chosen time. Settings: run as soon as possible after a missed start, run on battery, stop after 10 minutes. If the task can't be created, fall back to a per-user Run registry entry for the sign-in check, so the daily check happens at the next sign-in. Log which path was used.

**Each check, in order:**

1. Take a single-instance lock. If another check is running, exit. If the last successful check ended less than 30 minutes ago, exit (Check now ignores this).
2. Wait for the network for up to 10 minutes with backoff, since sign-in often happens before Wi-Fi is up.
3. Load the token; refresh it if it is over 7 days old. If that fails, mark disconnected, notify, exit.
4. Sync locations, catalog changes, new orders and inventory counts.
5. Recompute every label.
6. Write the labels that changed, using the safe-write procedure.
7. Record the time, counts and any errors for the status line. Notify only if the owner must act, or after 3 failed checks in a row.
8. Exit.

Phase 0 proves each trigger fires on both systems before anything else is built on it.

## Architecture

Tauri v2 for the window and a Rust core that does all the work, the same stack as the spotter and sorter, minus the model.

| Folder | Holds |
| --- | --- |
| `core/square/` | HTTP client with the host allowlist, OAuth PKCE, keychain token store, API calls, paging, retries and backoff |
| `core/sync/` | SQLite cache, order checkpoint, catalog snapshot |
| `core/compute/` | Last-sold dates, slow flags, label text |
| `core/labels/` | Attribute definition, the safe-write procedure, diffing |
| `core/schedule/` | Mac LaunchAgent and `SMAppService`, Windows Task Scheduler and the Run-key fallback, the single-instance lock |
| `core/notify/` | System notifications |
| `app/` | Tauri shell and the interface (TypeScript, no heavy framework) |
| `mock/` | A local mock Square server for tests |
| `gen/` | Seeded generator of catalogs and sales histories |
| `site/` | The static download page |

- **Three ways to start:** no flag opens the window; `--check` runs headless; `--self-test` runs a full check against bundled mock data and exits pass or fail. CI uses `--self-test`, and the interface exposes it as Help → Check this computer.
- **Pinned Square API version.** Every request sends a fixed `Square-Version` header; upgrading it is a deliberate change with a `DECISIONS.md` entry and a full test run.
- **One HTTP client for the whole app,** so the host allowlist can't be bypassed by another library.

## Tests

Four layers: a mock Square server for every edge case, a seeded generator with known answers, a real Square Sandbox account, and scheduler tests on real Mac and Windows runners.

**Mock Square server (every push).** Square's response shapes, replayed with these scenarios:

- Paging across many pages; rate-limit errors with retry delays; an expired access token; a rejected refresh token; a network drop mid-sync.
- Version conflicts on write; an object that comes back changed in more than the label.
- Deleted and archived items; an item with 50 variations; 20,000 items; 100,000 orders.
- Several locations in different time zones; a daylight-saving change.
- Orders closed or edited after the checkpoint; returns; canceled orders; custom-amount lines.

**Generator and property tests (every push, 200 seeds; nightly, 2,000).** Each seed builds a catalog and a sales history with known last-sold dates. These must hold for every seed:

- Every label equals the truth computed from the generator.
- Returns never move a last-sold date.
- A second check with no new orders writes nothing.
- Every outgoing write differs from the fetched object only in this app's key.
- Killing the check at any point and running it again gives the same final labels.
- No request goes to a host outside the allowlist.
- Tokens never appear in logs, the cache, exports or diagnostics.

**Square Sandbox (nightly and every release).** A dedicated Sandbox seller, seeded by script with items and orders. Run a check, read every label back, confirm the diff-only rule held, then remove all labels and confirm the catalog matches its starting snapshot exactly. CI uses a Sandbox access token stored as a secret, since PKCE needs a browser.

**Scheduler (every release, on real runners).** Install as a standard user, register the triggers, read the registration back (`launchctl print`, `schtasks /query /xml`), fire it (`launchctl kickstart`, `schtasks /run`), and confirm a check ran with no window and the lock stopped an overlapping run. CI can't truly reboot, so it checks that the sign-in trigger is registered; a real reboot is tested by hand on one Mac and one PC in Phase 7.

## CI and install tests

Cloud CI runners only, the same matrix as the spotter and sorter. ([GitHub runners](https://docs.github.com/en/actions/reference/runners/github-hosted-runners))

| Runner | Role |
| --- | --- |
| `macos-14` | Gating: full suite, scheduler tests, install test |
| `macos-26` | Newest macOS: full suite, scheduler and install tests |
| `macos-15-intel` | Intel Mac: self-test and install test from the universal build |
| `windows-2025` | Windows x64: full suite, scheduler tests as a standard user, install test |
| `windows-11-arm` | The x64 build under emulation on ARM: self-test and install test |
| `ubuntu-latest` | Core only: mock server, generator, property tests, allowlist trace |

**Install tests (every release candidate):**

- **Mac:** mark the disk image as downloaded the way a browser does (quarantine attribute), mount it, copy the app, confirm Gatekeeper accepts it as notarized, launch with `--self-test`, then register the background check and confirm it appears among the user's Login Items.
- **Windows:** mark the installer as downloaded (Mark of the Web), run it silently as a standard user, confirm it never asks for admin and its signature is valid, launch with `--self-test`, confirm the scheduled task exists under the user, then uninstall and confirm the task and the app are gone. The owner's cache is removed only if they chose to.
- **Allowlist:** on Linux, trace every outbound connection during a full mock run and fail on any host not on the list. On Mac and Windows, run `--self-test` with only the mock server reachable.

## Packaging, download and signing

Same delivery as the other tools: one download per platform from a page on the company's domain, hosted in the same Cloudflare R2 bucket. With no model the files are small.

**Mac**

- One universal app inside a signed disk image: Developer ID certificate, hardened runtime, notarized, ticket stapled.
- The background LaunchAgent ships inside the app bundle, so it is signed and notarized with it.
- On first open macOS shows its standard "downloaded from the internet" prompt once, and later a one-time notice that a background item was added.

**Windows**

- Tauri's NSIS installer in current-user mode, so no admin password, with the WebView2 offline installer embedded. ([Tauri](https://v2.tauri.app/distribute/windows-installer/))
- The installer and every .exe and .dll signed with Azure Artifact Signing and timestamped.
- **Expect a SmartScreen warning at first.** New downloads show "Windows protected your PC" until they build reputation, signed or not. Store installs never show it. Keep one signing identity and release rarely. The recovery card covers it: check the publisher name, then More info, then Run anyway. ([Microsoft](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/code-signing-options))

**Updates.** The app never checks for updates. A new version installs over the old one and must keep the token, the cache, the labels and the background check exactly as they were; an install test proves it.

**Download page.** The same static page pattern as the other tools: detects Mac or Windows, one big button, file size; on a phone, from the card's QR code, it explains the tool and offers a Share button. No tracking, cookies or forms.

## Privacy and security, enforced by tests

The company never receives the seller's token, items or sales; everything stays on the seller's computer and goes only to Square. So there is nothing on the company's side to breach.

- **Host allowlist.** One HTTP client, two allowed hosts, proven by the allowlist tests.
- **Least permission.** Write access to items is requested only when labels are on, and the app writes only its own attribute.
- **Tokens in the keychain only.** Never in files, logs, exports or diagnostics, proven by a property test.
- **Locked-down page.** Content Security Policy `default-src 'self'`; no remote scripts, fonts or images. The only network calls come from the Rust core, never the page.
- **No dependency phones home.** No telemetry, updater or crash-reporting plugins; a dependency allowlist check fails CI if one appears.
- **Export safety.** Any cell starting with =, +, -, @, tab or carriage return is written as text, so a saved file can't run anything in Excel.
- **Diagnostics are redacted.** Help → Save diagnostics replaces item names, amounts and IDs with placeholders and shows the file before saving.
- **Clean exit.** Disconnect deletes the token. Uninstall removes the background check, and asks before removing the cache.
- **Square's own control.** The seller can cut access at any time in Square Dashboard under Settings, then App integrations, Disconnect App. ([Square help](https://squareup.com/help/us/en/article/5411))

## Build phases and gates

Eight phases, each with a gate that must pass before the next starts. Phase 0 settles the two risky unknowns, Square's catalog writes and the background triggers, before anything is built on them.

1. **Phase 0, scaffold and verify.** Repo, CI skeleton, `DECISIONS.md`, Sandbox application. Confirm the PKCE localhost redirect, scope names, catalog write rules, label persistence and where labels appear (screenshots), and whether catalog objects carry a creation date. Prove each background trigger fires on both systems. **Gate:** a Sandbox seller connects through PKCE; one label written by the safe-write procedure shows in the Sandbox Dashboard; the daily and sign-in triggers are registered and fire on `macos-14` and `windows-2025`.
2. **Phase 1, Square client.** OAuth, keychain storage, refresh rotation, host allowlist, paging and backoff, the mock server. **Gate:** every mock scenario passes; the token survives a restart; a dead refresh token leads to "disconnected" and a notification.
3. **Phase 2, sync and calculation.** Cache, first sync, incremental sync, the last-sold rules, generator and property tests. **Gate:** 200 seeds at 100%; the speed and memory targets met.
4. **Phase 3, labels.** The attribute definition, the safe-write procedure, Remove all labels. **Gate:** the diff-only property holds on every seed; a Sandbox run changes nothing but labels; Remove all labels returns the catalog to its exact starting snapshot.
5. **Phase 4, headless check and scheduler.** `--check`, triggers, lock, network wait, notifications. **Gate:** scheduler tests pass on Mac and Windows as a standard user.
6. **Phase 5, the window.** Connect, setup, report, export, status line, settings, Check this computer. **Gate:** UI tests green on Windows and Linux; `--self-test` green on the real Mac bundle.
7. **Phase 6, packaging, signing and download.** Disk image, current-user installer, signing, install tests including an upgrade that keeps everything, the download page. **Gate:** install tests pass on every runner (unsigned allowed until secrets exist).
8. **Phase 7, field test.** With permission, a real Square account: a real reboot on one Mac and one PC, screenshots of labels in the Dashboard and point-of-sale app, a week of daily checks. **Gate:** Austin signs off.

## Deliverables

At the end of Phase 7, Claude Code hands over:

| Deliverable | Contents |
| --- | --- |
| Builds | Mac disk image and Windows installer, signed once secrets exist |
| `SQUARE_FACTS.md` | Phase 0's answers: redirect, scopes, catalog write rules, where labels show (with screenshots), creation dates |
| `TEST_REPORT.md` | Mock scenarios, generator results, Sandbox runs, scheduler and install tests, known failures |
| `DECISIONS.md` | Every open choice and deviation, dated, with the reason |
| `site/` and `RELEASE.md` | The download page, the upload script, and how to publish a new version |
| `SQUARE_APP_SETUP.md` | Step by step for Austin: creating the production Square application and its redirect URL |
| Recovery card | One page for salespeople: Mac and Windows security prompts, the Square approval screen, what to do if the connection lapses |
| Test library | `mock/` scenarios, the generator, the Sandbox seeding script |

## Known limits

- **The computer must be on and signed in** for labels to stay current. While it's off, labels freeze, but their dates keep them true.
- **Checks run at sign-in, not at power-on.** A machine left at the sign-in screen isn't checked until someone signs in.
- **The connection lapses after 80–90 days without a check,** because Square's PKCE refresh tokens expire after 90 days; the owner reconnects with one click.
- **Labels are account-wide.** Square custom attributes can't differ by location, so per-location detail lives only in the report.
- **Where labels appear is Square's choice.** Phase 0 records it; Square can change its screens later.
- **Every label write sends the item back to Square.** The safe-write procedure and tests guard it, and date-based labels keep writes rare, but it is the riskiest part of the app.
- **Other apps connected to Square may notice item updates** when a label changes.
- **It only sees sales that go through Square.** Sales rung up elsewhere don't count.
- **On Windows, new releases show a SmartScreen warning** until they build reputation.

## Sources

- [Square: OAuth overview, PKCE for desktop apps, token lifetimes](https://developer.squareup.com/docs/oauth-api/overview)
- [Square: redirect URLs and the PKCE port placeholder](https://developer.squareup.com/docs/oauth-api/create-urls-for-square-authorization)
- [Square: CatalogObject, versions and location-independent custom attributes](https://developer.squareup.com/reference/square/objects/CatalogObject)
- [Square: custom attribute seller visibility](https://developer.squareup.com/reference/square/enums/CatalogCustomAttributeDefinitionSellerVisibility)
- [Square developer forum: custom attribute values not persisting in Sandbox](https://developer.squareup.com/forums/t/custom-attribute-values-on-an-item-silently-dont-persist-sandbox/27075)
- [Square developer forum: no custom URI schemes for redirects](https://developer.squareup.com/forums/t/oauth-pkce-permission-redirect/15310)
- [Square developer forum: no embedded app screens in the Dashboard](https://developer.squareup.com/forums/t/can-a-square-marketplace-app-embed-its-own-ui-inside-the-square-dashboard/27806)
- [Square help: connecting and disconnecting third-party apps](https://squareup.com/help/us/en/article/5411)
- [Tauri v2: Windows installer](https://v2.tauri.app/distribute/windows-installer/)
- [Microsoft: code signing options and SmartScreen](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/code-signing-options)
- [GitHub-hosted runner specs](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)
