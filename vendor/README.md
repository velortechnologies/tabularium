# Vendored dependencies

## sqlx-sqlite 0.8.6

Copied from eva-ics/eva4 commit 02f3888 (introduced by 5ff105c),
`vendor/sqlx-sqlite`. Upstream: https://github.com/launchbadge/sqlx,
MIT OR Apache-2.0 (license files included).

The driver retains the SQLx 0.8 API and uses libsqlite3-sys 0.39.1,
addressing RUSTSEC-2026-0459 without upgrading the whole SQLx stack.
The workspace patches only sqlx-sqlite; other dependencies are unchanged.
