# Binary attachments validation — 2026-11-11

## Test migration

- Source: tb-test 10.90.1.100; destination: citests 10.90.1.71.
- Consistent stopped-service archive retained on both hosts at
  `/root/tb-migration-20261111/source.tgz` (mode-private parent directory).
- Archive SHA-256: `f7c2f5aa1e20837e5263df432be72c34d39ae5123e599fad5ce5b16358d529ba`.
- Started the old binary on the destination before replacing server and CLI.
- Old/new database comparison: 3291 entries, identical ordered
  `(id,parent_id,kind,name,content,revision,modified_at)` digest
  `333d32cb78844746ecf050d4be42e7e68fd3f19110084e6b827c9f7a3cc664c9`.
- `PRAGMA quick_check=ok`; content_kind, sha256, blob_revision each present once.
- UID/GID mapping corrected for data and config on the new host.
- Source data rechecked unchanged, then old service stopped and disabled.
  Source container retained for owner deletion. Production .77 untouched.

## Live checks

- HTTP raw byte equality, HEAD length, Range/If-Range, revision-aware replacement.
- RPC metadata and Base64 roundtrip; unauthenticated media rejected.
- Real MCP initialize, tools/list and get_binary successful.
- Interrupted HTTP upload with a declared larger Content-Length publishes no entry.
- Two retained entries: `/binary-smoke-20261111/pixel.png` and `bytes.bin`.
  Exactly two current blob files; replaced generation removed.
- Browser session persists across server restart; cookie media access succeeds;
  logout revokes access. Temporary cookie file removed.
- Reproduction script on test host: `/root/tb-migration-20261111/smoke.py`.
  It obtains credentials locally without logging them; create-only root means
  it is not intended for blind repeated execution.

## Automated checks

- Prior workspace debug Rust suite passed (v1 `/tmp/tabularium-binary-tests.log`).
- Release build and embedded UI build passed on .71.
- CI binary import/export + metadata sidecars and replacement test added.
- Selenium tall/wide PNG containment tests added and passed on .71.
- Full Python run: 194 passed, 1 skipped, 2 failures in pre-existing wc output
  expectations. Those expectations were corrected (GNU count-before-total and
  single-match flat output); isolated repeat of the entire wc module: 4 passed.
  The full run was not repeated; its original nonzero result remains in the log.
- Complete `just test` release Rust suite passed on .71, log
  `/tmp/tabularium-rust-release.log`. `cargo fmt --all --check` passed.
- `cargo clippy --workspace --all-targets` completed successfully with warnings;
  the owner's `clippy` wrapper is absent on v1. Log:
  `/tmp/tabularium-binary-clippy.log`.
- Final installed server SHA-256:
  `90e046a81610631d35dcc6d07b8733c56a0ba93744e84516256d591743a851fe`.
  Served HTML/JS contain the new session/media UI; final HTTP check and all
  original row comparisons passed after the final restart.
- Python suite log: v1 `/tmp/tabularium-binary-ci.log`.

## CI browser prerequisite

Ubuntu's chromium-browser package attempted an unreachable Snap Store and
waited for 30 minutes. CI now uses preinstalled Chrome for Testing in
`/opt/chrome-test` (matching chromedriver), or native Chromium. Browser
installation failure is explicit, not an indefinitely retrying snap bootstrap.
`CHROME_BINARY` allows the fixture to select the actual browser.

## Scope still not independently exercised

Video playback and seeking were subsequently covered by the authenticated
audio/video smoke below. Range behavior is covered by HTTP tests.
The existing 32 MiB RPC JSON limit remains; the streamed content route has no
application upload limit. No production deployment, release or version bump.

## Follow-up: download button and real photo

Removed the duplicate inline Download link. Uploaded an owner-requested selfie
frame as `/binary-smoke-20261111/kira-selfie.jpg`. Rebuilt and deployed the UI/server.
Selenium on .71 authenticated via cookie, loaded the actual image, confirmed
no lower Download link, clicked the toolbar download button and compared the
saved JPEG byte-for-byte (SHA-256
`1c26bdd7f6ece6e904899d88558b358f948bf25f356ea6e8b59636fcb026527b`).
Test script on .71: `/root/tb-migration-20261111/check-download.py`.


## Audio/video follow-up

Added native audio controls for Ogg/Opus, MP3, MP4 audio, WAV, WebM audio and
FLAC; the content route serves these audio types inline. No autoplay.
Retained owner-authorized test entries: voice-smoke.ogg (1.66 seconds,
Telegram archive message 1714166, connection-check phrase) and
kira-selfie-video.mp4 (5 seconds), under /binary-smoke-20261111.
Authenticated Chrome check verified metadata load, initially paused controls,
playback clock advancement, seek/resume and HTTP 206 inline reads for both.
Script: .71 /root/tb-migration-20261111/check-av.py.
CI includes generated WAV playback/seek alongside the tall/wide PNG tests.
