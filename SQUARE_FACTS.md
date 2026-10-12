# Square facts (Phase 0)

What the app relies on from Square, and where each fact came from. Each answer has a status:
- **Docs**: settled from Square's documentation (2026-10-12): the OpenAPI spec at Release 2026-09-16, the API reference and guides, the changelog, and forum answers from Square staff.
- **Sandbox** or **Production**: still waiting on a check against a real Square account. Austin has no Square developer account yet, so these stay open until he does; `SQUARE_APP_SETUP.md` has the steps.

Decisions that follow from these facts are in `DECISIONS.md`.

## The four Phase 0 questions on catalog writes

| Question | Answer | Status |
|---|---|---|
| What must a catalog write include so nothing else changes? | **The whole object, exactly as fetched.** Square's upserts are full replacement: "any field absent from the request is interpreted as an intentional clear. This logic applies to nested objects as well… omitting inlined children like variations will delete them." The app sends every fetched field back (raw JSON, field order and number formats kept), changing only its own key. Reads and writes use the same pinned `Square-Version`, because a field added between the two versions would be cleared. | Docs. Sandbox still to confirm. |
| Do values persist? | The forum report of values silently not saving in Sandbox (thread 27075) is unresolved. Square staff say custom attributes "are operating as expected in sandbox" and asked for a unique idempotency key on every attempt. The app uses a fresh idempotency key per attempt (and the same key only when resending an identical request after a dropped connection), and reads every write back. | Docs. Sandbox still to confirm. |
| Where does the label appear? | Square Dashboard, on the item's Edit Item page. Variation values appear in the variation panel. Sellers can search by the attribute. **Square's point-of-sale app shows no custom attributes for any object type.** No Item Library column or filter is documented. | Docs. Sandbox screenshots still needed. |
| Item level, variation level, or both? | Both show in the Dashboard. The app writes the item level always, and the variation level when an item has two or more variations. | Docs. Sandbox screenshots still needed. |

## OAuth (PKCE)

- **Authorize URL.** `https://connect.squareup.com/oauth2/authorize`, or `connect.squareupsandbox.com` for Sandbox.
  - Parameters sent: `client_id`, `scope` (space-separated), `state`, `code_challenge`, `redirect_uri` and `session=false`.
  - `session=false` is required in production and ignored in Sandbox.
  - No `code_challenge_method`: Square documents none, and S256 is implied.
  - In Sandbox, the test seller's Dashboard must be open in the same browser.
- **Token endpoint.** `POST /oauth2/token`, with no client secret.
  - First exchange: `client_id`, `grant_type=authorization_code`, `code`, `code_verifier`, `redirect_uri`.
  - Refresh: `client_id`, `grant_type=refresh_token`, `refresh_token`.
  - The response has `access_token`, `expires_at`, `merchant_id`, `refresh_token`, and `refresh_token_expires_at` (PKCE only).
- **Lifetimes.**
  - Access tokens last 30 days.
  - PKCE refresh tokens are single-use and expire after 90 days. Every refresh returns a new one, which must replace the old one at once.
  - Square recommends refreshing every 7 days or less; the app refreshes when the token is over 7 days old.
  - Authorization codes expire after 5 minutes.
- **Redirect URL.** Square documents registering `http://localhost:<port>`, with the literal `<port>`, for PKCE clients that pick a port at runtime.
  - Other pages say production redirects must be HTTPS, so whether production accepts `http://localhost` is open until checked in production.
  - Only "localhost" is documented, not 127.0.0.1.
  - Custom URL schemes are not supported.
  - Status: Docs, contradictory; Production still to confirm.
- **Revoke.** It needs the client secret, so a PKCE app can't revoke its own token. Disconnect deletes the token locally and points the owner to the Dashboard.
- **Scopes.** All of these are valid and enough:
  - `MERCHANT_PROFILE_READ`: merchant and locations;
  - `ITEMS_READ`: catalog reads;
  - `ORDERS_READ`: SearchOrders;
  - `INVENTORY_READ`: inventory counts;
  - `ITEMS_WRITE`: catalog upserts, including creating the attribute definition.

## Catalog

- **Square-Version.** Pinned to `2026-09-16`, the latest. It must be at least `2026-07-15` for unit cost.
- **Creation date.** `CatalogObject.created_at` is not in the documented schema at any version, though real responses sometimes include it (one Sandbox sample was dated 1970). Undocumented fields "can change or be removed without notice". The app uses the date it first saw an object.
- **Fields the app reads.**
  - Item: `item_data.name`; `reporting_category` / `categories[]` (`category_id` is deprecated); `is_archived`; `variations[]`; the presence fields (`present_at_all_locations`, default true; `present_at_location_ids`; `absent_at_location_ids`).
  - Variation: `item_variation_data.price_money`; `pricing_type` (FIXED_PRICING / VARIABLE_PRICING); `track_inventory`; `location_overrides[].track_inventory`.
  - Unit cost: `item_variation_data.vendor_information[0].unit_cost_money` ("the first entry is the default").
- **Custom attribute definition** (one per seller, created on first use):
  - `type` STRING; `name` "Last sold" (unique per merchant and app); `description`.
  - `allowed_object_types` [ITEM, ITEM_VARIATION].
  - `seller_visibility` SELLER_VISIBILITY_READ_WRITE_VALUES; `app_visibility` APP_VISIBILITY_HIDDEN.
  - `key` `dead_stock_last_sold`: 1–60 characters `[a-zA-Z0-9_-]`, unchangeable.
  - String values are up to 255 characters.
  - A seller may have at most 10 seller-visible definitions. If that limit blocks creating ours, the app runs report-only for that seller and says why.
- **Values.**
  - The owning app sees its values under the plain key.
  - Other apps' values come back as `<app-id>:<key>`, and the seller's own as `Square:<uuid>`. The app sends these back untouched.
  - The minimum value written is `string_value`; the app also sends `name`, `key`, `type` and `custom_attribute_definition_id`, as Square's guide does.
  - A value is cleared by leaving its entry out (full replacement).
- **Concurrency.**
  - Every write carries the object's version, and an item write carries each nested variation's version too.
  - Since 2026-07-15, Square three-way merges a write made with an old version: non-overlapping changes merge silently, and `VERSION_MISMATCH` comes back only when both sides changed the same field.
  - Square processes one write at a time per seller; others get 429.
  - A failed write can still partly commit, which is why the app re-reads before deciding anything.
- **Batch writes.**
  - Up to 1,000 objects per batch and 10,000 per request; each batch is all or nothing.
  - Square publishes no rate limits: back off with jitter on 429, and don't run writes in parallel.
- **Reads.**
  - SearchCatalogObjects: `limit` up to 1,000 (advisory).
    - `begin_time` is exclusive; `include_deleted_objects` returns tombstones.
    - The response has `latest_time`.
    - ITEM_VARIATION isn't in its default types.
  - ListCatalog pages are fixed at 100 and return no deleted objects, so the app doesn't use it.
  - BatchRetrieveCatalogObjects returns items with their variations.
- **Writing a variation alone** is possible, and bumps both the variation's and its item's version.

## Orders

- **SearchOrders.**
  - At most 10 `location_ids` per call; `limit` up to 1,000.
  - `state_filter`; `date_time_filter` on `created_at`, `updated_at` or `closed_at`, with an inclusive range.
  - The sort field must match the filter field, and `closed_at` needs a state filter.
- **POS orders.** Square says orders "include all sales, returns, and exchanges regardless of how… they entered the Square ecosystem (such as Point of Sale…)".
  - Offline POS sales can reach Square up to 72 hours late.
  - POS orders have no `version`.
- **Line items.** `catalog_object_id` (the variation); `quantity` (a decimal string); `item_type` (ITEM, CUSTOM_AMOUNT or GIFT_CARD).
- **Returns.** Returns and refunds create separate return orders (`returns[].return_line_items`), so a sale's lines never change. Exchanges put new line items and returns on the same order.

## Inventory

- **Endpoint.** `POST /v2/inventory/counts/batch-retrieve` (the old `batch-retrieve-counts` path is deprecated). Request fields: `location_ids`, `states` [IN_STOCK], `updated_after`, `limit` (up to 1,000), `cursor`.
- **Counts.** Quantities are decimal strings. No count means the variation never had stock in that state. An UNTRACKED state exists since 2026-07-15.

## Locations and merchant

- **ListLocations.** Includes inactive locations (`status` ACTIVE / INACTIVE), each with an IANA `timezone`.
- **Merchant.** `GET /v2/merchants/me` gives `business_name` and `main_location_id`.

## Sandbox

- **Tokens for CI.** A Sandbox access token comes from the Developer Console: the default test account's token is the application's Sandbox access token. The Console can also authorize test accounts per scope without running OAuth.
- **Limits.** Sandbox has no point-of-sale app, so test orders are created through the API. Refunds can be viewed but not issued in the Sandbox Dashboard.
- **Custom attributes.** Whether the Sandbox Dashboard shows catalog custom attributes is undocumented. Phase 0's screenshots settle it.

## Sources

- https://github.com/square/connect-api-specification (Release 2026-09-16)
- https://developer.squareup.com/docs/oauth-api/overview
- https://developer.squareup.com/docs/oauth-api/create-urls-for-square-authorization
- https://developer.squareup.com/docs/oauth-api/refresh-revoke-limit-scope
- https://developer.squareup.com/reference/square/oauth-api/obtain-token
- https://developer.squareup.com/docs/oauth-api/square-permissions
- https://developer.squareup.com/docs/catalog-api/add-custom-attributes
- https://developer.squareup.com/docs/catalog-api/manage-vendor-information
- https://developer.squareup.com/docs/catalog-api/sync-with-external-system
- https://developer.squareup.com/docs/orders-api/manage-orders/search-orders
- https://developer.squareup.com/docs/orders-api/order-returns-exchanges
- https://developer.squareup.com/docs/build-basics/handling-errors
- https://developer.squareup.com/docs/devtools/sandbox/overview
- https://developer.squareup.com/docs/changelog/connect
- Forum threads 27075, 27162, 28130, 27742, 25876, 15310, 11267, 6561, 5897
- https://squareup.com/help/us/en/article/5411
