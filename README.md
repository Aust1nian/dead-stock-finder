# Dead Stock Finder

A free desktop app from Tight Line for Square sellers. It finds items that haven't sold in a window the owner picks, and writes a "Last sold" label onto each item in Square. It connects with the owner's own Square login, keeps everything on their computer, and refreshes itself in the background every day and at every sign-in.

- Spec: [`SPEC.md`](SPEC.md)
- Choices and deviations: [`DECISIONS.md`](DECISIONS.md)
- Square facts checked in Phase 0: [`SQUARE_FACTS.md`](SQUARE_FACTS.md)

| Folder | What's in it |
|---|---|
| `core/square/` | The one HTTP client and its host allowlist, OAuth PKCE, keychain token store, Square API calls |
| `core/sync/` | SQLite cache, order checkpoint, catalog snapshot, the report |
| `core/compute/` | Last-sold dates, slow flags, label text (pure functions) |
| `core/labels/` | The "Last sold" attribute and the safe-write procedure |
| `core/check/` | The check every trigger runs, status, the self-test |
| `core/schedule/` | macOS LaunchAgents, Windows Task Scheduler, the single-instance lock |
| `core/notify/` | System notifications |
| `app/` | Tauri shell and the page (TypeScript, no framework) |
| `mock/` | Local mock Square server |
| `gen/` | Seeded generator of catalogs and sales histories with known answers |
| `site/` | The download page |
| `tools/` | Dependency check, CI scripts, Linux dev image |

## Develop

```sh
cargo test                                  # core, mock scenarios, generator
python3 tools/depcheck.py                   # nothing but Square in the shipped tree
tools/docker/run.sh "cd app && npm ci && npx tauri build --no-bundle"   # Linux build without system packages
```
