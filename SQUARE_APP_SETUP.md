# Setting up the Square application (for Austin)

Dead Stock Finder connects to each seller's Square account through a Square application that belongs to Tight Line. The application's name is what sellers see on Square's approval screen, so use the company's account, not a personal one.

You'll do this twice: once for Sandbox (testing, needed for Phase 0's remaining checks) and once for production (before sellers use it). Neither costs money.

## 1. Create the developer account and application

1. Go to https://developer.squareup.com and sign in with the company's Square account, or create one.
2. Open the **Developer Console** and choose **Create an application**.
3. Name it **Dead Stock Finder** (or the final product name). Sellers see this name on Square's approval screen.

## 2. Sandbox (for testing)

1. In the application, switch the toggle at the top to **Sandbox**.
2. Open **OAuth** in the left menu.
3. Under **Redirect URL**, enter exactly: `http://localhost:<port>`
   (type the word `<port>` literally, angle brackets included. Square fills in the port the app picks each time.)
4. Save.
5. Open **Credentials** and copy the **Sandbox Application ID** (it starts with `sandbox-sq0idb-`). This isn't secret; paste it to Claude Code.
6. On the same page, copy the **Sandbox Access token**. This one **is** secret. Add it to GitHub yourself, so it never passes through chat:
   ```
   gh secret set SQUARE_SANDBOX_TOKEN --repo Aust1nian/dead-stock-finder
   ```
   and paste the token when asked. CI uses it for the nightly Sandbox run.
7. Open **Sandbox test accounts** and choose **Open in Square Dashboard** for the default test account. Keep that tab open: Square requires the test seller's Dashboard to be open in the same browser when the app connects.

## 3. Production (before sellers use it)

1. Switch the toggle to **Production**.
2. Under **OAuth → Redirect URL**, enter `http://localhost:<port>` and save.
   - **If Square refuses it** (its docs disagree on whether production allows `http://`), tell Claude Code. The fallback is a small HTTPS page on the company's website that passes the sign-in code back to the app. The code is useless without a secret that never leaves the seller's computer.
3. Copy the **Production Application ID** (it starts with `sq0idp-`). It isn't secret. Release builds have it built in.
4. Leave everything else as it is. The app uses PKCE, so it never needs the application secret. **Don't share the secret with anyone**, Claude Code included.

## 4. What the app asks sellers for

On Square's approval screen, sellers see these permissions:

| Permission | Why |
|---|---|
| Merchant profile (read) | Business name and locations |
| Items (read) | Items, variations, prices, costs |
| Orders (read) | Which items sold, and when |
| Inventory (read) | Units on hand, for the value tied up |
| Items (write) | Only when labels are on: writing the "Last sold" label |

## 5. What's still open until this is done

- Sign-in through Square's real page (PKCE), and whether production accepts the `localhost` redirect.
- One label written to a Sandbox item and seen in the Sandbox Dashboard, with screenshots.
- Whether a write must include anything beyond what the app sends, and whether values persist.
- The nightly Sandbox test run in CI.
