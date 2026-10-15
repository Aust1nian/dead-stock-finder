# Releasing Idle Stock Finder

Each release is a Mac disk image and a Windows installer, published on the company's download server at `get.tightline.systems`. The tool's page on tightline.systems (`site/data/tools/idlestock.json` in the `tightline-systems` repo) picks it up from there.

The app never checks for updates. Owners install a new version over the old one, which keeps their connection, cache, labels and background check (the install tests prove it).

## 1. Build a release candidate

1. Bump `version` in `app/src-tauri/tauri.conf.json` and `[workspace.package]` in `Cargo.toml`. Commit.
2. In GitHub, open **Actions → release → Run workflow** and enter the version (for example `0.2.0`). Pushing a tag `v0.2.0` does the same.
3. The workflow builds:
   - `IdleStockFinder-<version>-mac.dmg`: universal, for Apple Silicon and Intel;
   - `IdleStockFinder-<version>-win-x64.exe`: the current-user installer with WebView2 included.
4. It also builds the previous version and runs the install tests on `macos-15`, `macos-26`, `macos-15-intel`, `windows-2025` and `windows-11-arm`:
   - downloaded-file marking (quarantine / Mark of the Web), Gatekeeper and signature when signed, and no admin prompt;
   - `--self-test` from the installed copy, and background-check registration;
   - an upgrade over the previous version that keeps everything;
   - on Windows, uninstall.
5. The run summary lists each file's real size and SHA-256. Download the `macos-dmg` and `windows-nsis` artifacts.

Don't publish if any install test failed. The JSON results are attached to the run.

## 2. Signing

Builds are signed when these repository secrets exist, and unsigned until then.

| Platform | Secrets | Status |
|---|---|---|
| macOS: Developer ID, hardened runtime, notarized, stapled | `APPLE_CERTIFICATE` (base64 .p12), `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_ID`, `APPLE_PASSWORD` (app-specific), `APPLE_TEAM_ID` | Waiting on Austin's Apple Developer account details |
| Windows: Azure Artifact Signing, timestamped | `AZURE_TENANT_ID`, `AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET`, `AZURE_SIGNING_ENDPOINT`, `AZURE_SIGNING_ACCOUNT`, `AZURE_CERT_PROFILE` | Open decision (tightline-systems QUESTIONS.md, S1) |
| Square production app id | `SQUARE_APP_ID` (not secret, but kept with the others) | After `SQUARE_APP_SETUP.md` step 3 |

Add each secret with `gh secret set <NAME> --repo Aust1nian/idle-stock-finder`. Keep one Windows signing identity and release rarely: SmartScreen reputation builds per identity.

Without `SQUARE_APP_ID`, a build opens but can't connect ("This build isn't linked to a Square application yet"). Never publish such a build.

## 3. Publish

On the workstation, in the `tightline-systems` repo:

```sh
cd scripts
node release.mjs --tool idlestock --version 0.2.0 \
  --mac ~/Downloads/IdleStockFinder-0.2.0-mac.dmg \
  --windows ~/Downloads/IdleStockFinder-0.2.0-win-x64.exe --dry-run
node release.mjs --tool idlestock --version 0.2.0 --mac … --windows …
node recovery-card.mjs
cd .. && deploy/deploy.sh
```

`release.mjs` hashes each file and copies it to the download server. It updates `releases.json` and keeps this version and the one before it. Check the tool page afterwards with `deploy/check-live.sh`.

When the size changes a lot, update `plate.sizeUntilRelease` in `site/data/tools/idlestock.json`. The Windows installer carries the WebView2 runtime, about 150 MB (DECISIONS.md, Packaging).

## 4. After publishing

- Install the published files on one Mac and one PC from the download page, the way an owner would.
- Record the release in `TEST_REPORT.md`: version, run link, sizes, install-test results.
