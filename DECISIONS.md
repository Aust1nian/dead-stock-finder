# Decisions

Every judgment call and every deviation from `SPEC.md`, with the date, the choice, the reason, and what it rules out. Newest entries go at the bottom of each section.

## Austin's open decisions (SPEC § "Open decisions for Austin")

**2026-10-12 · 1. App name and branding: working default, "Dead Stock Finder", a free tool from Tight Line.** The company name comes from Austin's answer for the Schedule Spotter and Document Sorter. Bundle identifier: `com.tightline.deadstockfinder`. *Rules out:* nothing yet; Austin can rename.

**2026-10-12 · 2. Square developer app owner: working default (the company's Square developer account).** Austin has no Square Developer account yet. He chose to build against the mock server first ("Not yet, use the mock"). `SQUARE_APP_SETUP.md` gives him the steps for when he does. *Rules out:* nothing.

**2026-10-12 · 3–6. Default window 90 days, label wording, labels on by default, check at 5:00 a.m.: working defaults, as specified.** Refinements are logged under Labels and Scheduling below.

**2026-10-12 · 7. Microsoft Store: no for v1** (spec default).

## Repo and CI

**2026-10-12 · Repo: a public GitHub repository** (Austin's answer). Standard GitHub-hosted runners, macOS included, cost nothing on public repos, so CI is $0. No license file grants reuse; the code is all rights reserved by default.

**2026-10-12 · Repo location.** The repo is initialised in the folder that held the spec. The spec is committed unchanged as `SPEC.md`, as the spec asks. The original file stays beside it untouched.

**2026-10-12 · Reasoning effort.** The spec asks for the highest reasoning effort. This session runs Claude Opus 5.5 at "xhigh", the level Austin selected, one step below "max". Recorded as a deviation, not changed.

**2026-10-12 · No shared code with the other gifts.** Austin's rule for the Schedule Spotter and Document Sorter: "Each gift is a stand alone product and may share similar frameworks but must never co mingle in the code." Patterns from those repos are re-implemented here, never imported.

## Phase 0: Square facts (checked against Square's docs, 2026-10-12)

Sources: Square's OpenAPI spec (`square/connect-api-specification`, Release 2026-09-16), the API reference, guides, changelog, and developer-forum answers from Square staff. Full detail is in `SQUARE_FACTS.md`. Sandbox and production checks are still open: Austin has no Square developer account yet.

**2026-10-12 · Square-Version pinned to `2026-09-16`, the latest.** It must be at least 2026-07-15, the version that added unit cost (`vendor_information`). Square also clears any field added between the version an object was read at and the version it is written at, so reads and writes must use the same version. The one HTTP client sends this version on every request.

**2026-10-12 · Catalog writes replace the whole object (spec assumption confirmed, with consequences).** Square has no endpoint that sets only a custom attribute value on a catalog object. An upsert is full replacement: any field left out is cleared, including nested variations. Every label write therefore sends the complete item exactly as fetched, with every unknown field kept:
- objects are held as raw JSON;
- `serde_json` runs with `preserve_order` and `arbitrary_precision`;
- the label key is the only thing changed;
- `diff::only_label_changed` proves it before sending.

**2026-10-12 · Version conflicts: Square now merges, so the spec's "Square rejects a write whose version doesn't match" is out of date.** Since 2026-07-15, a write with an old version is three-way merged: changes that don't overlap merge silently, and `VERSION_MISMATCH` comes back only when both sides changed the same field. Consequences:
- The safe-write procedure keeps the version and the 3-attempt retry, as specified.
- The read-back can now see a staff edit that was merged in while the label was being written. Step 5 can't tell that apart from damage, so it stops all writes and tells the owner. This is fail-safe: a rare false alarm (an edit landing within about a second of a label write) costs the owner one click to resume.
- One refinement keeps false alarms down. If Square's write response matches what was sent but the read-back differs, the change came from a later edit, which is logged and does not stop writes. A write response that differs from what was sent always stops writes.

**2026-10-12 · Labels written at item level always, and at variation level for items with two or more variations.** Square documents that item and variation custom attributes both show on the Dashboard's Edit Item page, so both levels qualify under "write only the levels that show". On a single-variation item, the variation's label would only repeat the item's. One item upsert carries both levels, so there is one write per item. Sandbox screenshots still need to confirm this (open until Austin's account exists).

**2026-10-12 · Square's point-of-sale app never shows custom attributes.** Square's docs: "The Square Point of Sale application doesn't show custom attributes for any object type." Labels appear only in the Square Dashboard (Edit Item page, and searchable). This narrows the spec's "staff see it in Square's own screens". Added to Known limits. Austin should know before Phase 7.

**2026-10-12 · No creation date for catalog objects: first-seen is used.** `created_at` is not in Square's documented `CatalogObject` schema at any version. Real responses sometimes carry it, and one Sandbox sample was dated 1970. Square says undocumented fields "can change or be removed without notice". The spec's rule ("if Square provides a creation date, use that") therefore doesn't apply. `dsf-compute` can take a creation date, but sync always passes none.

**2026-10-12 · A desktop (PKCE) app can't revoke its own token.** RevokeToken needs the client secret. Disconnect deletes the token from the keychain and points the owner to Square Dashboard → Settings → App integrations, as the spec already says.

**2026-10-12 · OAuth details.**
- `code_challenge_method` is not sent, because Square documents no such parameter (S256 is implied).
- `session=false` is sent; it is required in production and ignored in Sandbox.
- The redirect is `http://localhost:<port>`. The listener binds 127.0.0.1, and also `[::1]` on the same port when it can, because browsers may resolve `localhost` to either.
- Whether production accepts an `http://localhost` redirect is still open: Square's docs contradict each other. Austin's production check settles it. The fallback (an HTTPS page on the company domain) is unchanged from the spec.

**2026-10-12 · Unit cost comes from `item_variation_data.vendor_information[0].unit_cost_money`** ("the first entry is the default"). Reading it needs no special plan.

**2026-10-12 · Square limits the app works within.**
- SearchOrders takes at most 10 locations per call, so calls are chunked.
- ListCatalog pages are fixed at 100, so the catalog is read with SearchCatalogObjects (up to 1,000 per page) instead.
- Inventory counts come from the new path `POST /v2/inventory/counts/batch-retrieve`; the old one is deprecated.
- POS orders have no `version`, so duplicates collapse by order id with the newer `updated_at` winning.
- Returns arrive as separate return orders and never touch a sale's line items.
- A seller may have at most 10 seller-visible custom attributes. If creating the "Last sold" definition fails on that limit, the app says so and runs report-only for that seller.

## Architecture

**2026-10-12 · Each `core/` folder is its own crate**: `dsf-square`, `dsf-sync`, `dsf-compute`, `dsf-labels`, `dsf-schedule`, `dsf-notify`, plus `dsf-check`. The boundaries are enforced by the compiler:
- only `dsf-square` depends on the HTTP library;
- `dsf-compute` does no I/O.

**2026-10-12 · Added `core/check/`.** The spec's folder table has no home for the check itself (steps 1–8). The window's Check now, `--check` and `--self-test` all run this one crate.

**2026-10-12 · HTTP client: `ureq` 3 (blocking) with rustls.** The app is small and runs on threads, so no async runtime is needed.
- No proxy: ureq's default reads proxy environment variables, which would route requests through another host.
- No redirects followed, and no cookies.
- Roots of trust come from the operating system's store, so antivirus or company TLS inspection keeps working, with Mozilla's roots as a fallback. Online revocation checks are deliberately not used, because they would contact certificate authorities, which are hosts off the allowlist.

**2026-10-12 · The mock environment is loopback-only and exists in shipped builds.** `--self-test` runs a full check against bundled mock data, so the shipped binary can talk to an `http://127.0.0.1:<port>` mock server that it starts itself. The allowlist accepts exactly that address and port, and only when the mock was started in-process. `connect.squareupsandbox.com` is accepted only in builds with the `sandbox` feature (development), as the spec says.

**2026-10-12 · Keychain access is written directly against the OS APIs, not through the `keyring` crate.**
- macOS: Security framework via `security-framework`.
- Windows: Credential Manager via `windows-sys`.
- Linux, development only: the kernel's per-user keyring.

The reason is that background checks must never show a keychain prompt. On macOS, a headless run sets `kSecUseAuthenticationUI = Fail`, so a locked keychain makes the check fail quietly (and retry) instead of prompting. `keyring` 4 doesn't expose that setting.

**2026-10-12 · Orders are cached for every location, not just the selected ones.** Selection is applied when computing. Changing which locations count then never needs a re-read of history, and the report can show any location. It reads more data than the spec's "completed orders at the selected locations", but nothing leaves the computer.

**2026-10-12 · Checkpoints use Square's timestamps, never this computer's clock.**
- The order checkpoint is the newest `updated_at` seen; each check reads from the checkpoint minus one day.
- The catalog checkpoint is Square's `latest_time`, or else the newest `updated_at`, also read with a one-day overlap.

A computer clock that runs fast or slow can't make the app skip orders.

## Labels and the last-sold rules

**2026-10-12 · "Slow" means more than `window` days since the last sale** ("older than the window"). With a 90-day window, an item that last sold exactly 90 days ago is not yet slow.

**2026-10-12 · Dates are calendar dates at the selling location.** "Days since" counts calendar days in that location's time zone, so daylight-saving changes never shift a label. "Added" dates use the main location's time zone.

**2026-10-12 · "No sales in N+ months" is fixed at the first sync's depth.** N is 12, or more when the window plus 30 days needs it, rounded up to whole calendar months so the claim is always true. As the cache collects more history, N doesn't grow, so labels aren't rewritten every month for no new information. If the owner later widens the window past the history read, older months are read and N grows once.

**2026-10-12 · A new item that has existed longer than the window with no sales reads `SLOW · no sales since added Aug 2, 2026`.** The spec's wording gives the not-yet-slow form; this is its slow counterpart.

**2026-10-12 · Items and variations carried at none of the selected locations get no label**, and a label this app wrote earlier is removed. Archived items are skipped and left as they are.

**2026-10-12 · The label key is `dead_stock_last_sold`.** It can never change once sellers have labels: a new key would leave every old value behind.
