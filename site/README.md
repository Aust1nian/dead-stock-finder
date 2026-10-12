# Download page

Dead Stock Finder's download page is part of the company website, not this repo: **tightline.systems/deadstock**, built by the `tightline-systems` repo from `site/data/tools/deadstock.json`. Releases go to `get.tightline.systems` with that repo's `scripts/release.mjs` (see `RELEASE.md` here).

The page follows the spec: it detects Mac or Windows and shows one big button with the file size. On a phone, which is how owners arrive from the card's QR code, it explains the tool and offers a Share button. No tracking, cookies or forms.

The company moved downloads from Cloudflare R2 to its own server (`get.tightline.systems`) for every tool; that decision lives in the `tightline-systems` repo (QUESTIONS.md, C1). Keeping the page there means each gift's repo holds only its own code, the "no co-mingling" rule from Austin.

Facts the page states that come from this repo, to keep in step when they change:

| Page field (`deadstock.json`) | Now | Source here |
|---|---|---|
| `plate.sizeUntilRelease` | "Tens of MB" | Release summary sizes. The Windows installer embeds WebView2, about 150 MB (DECISIONS.md, Packaging) |
| `osRequirements` | macOS 11+, Windows 10 22H2 / 11 x64 | `tauri.conf.json`, SPEC.md |
| `support.slow` | First sync up to 10 minutes, daily check under a minute | TEST_REPORT.md speed results |
| `support.uninstallExtra` | Disconnect, then remove in Square Dashboard | The app's Settings |
| Recovery card | Mac/Windows prompts | Add the Square approval screen and "connection lapsed" from `docs/RECOVERY_CARD.md` |
