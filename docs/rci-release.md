# RCI release

`just release` launches and follows `rci x tabularium`. The version comes from
`[workspace.package]` in `Cargo.toml`; bump and commit it before invoking release.
Existing remote version tags are rejected before building or publishing.

## Pipeline

The tracked job is `ci/tabularium.yml`, deployed with:

```sh
rci job deploy tabularium < ci/tabularium.yml
```

The runner is lab-rci, Unix user `rci`, HOME `/opt/rci`, workspace
`/work/tabularium`. Node 22 is installed/selected through existing NVM without
changing other jobs' defaults. The job builds the UI, runs workspace tests with
`cargo test --workspace --release`, then invokes `just do-release`.

`do-release` performs:

1. Duplicate-tag and tool preflight, then `cargo audit`. Vulnerabilities block;
   informational warnings remain visible but nonblocking (owner decision,
   2026-11-21).
2. Cross-build and package CLI/server DEBs for amd64 and arm64.
3. Stage those exact binaries into Docker and execute CLI/server `--version`
   inside **both** runtime architectures, without pushing. Any runtime-loader
   failure stops the pipeline before public writes.
4. Add all four DEBs to signed APT, commit full generated `repo` state with
   message `tb VERSION`, push Git, then publish and verify public bucket files.
5. Push the validated multi-arch Docker image under `VERSION` and `latest`.
6. Create/push the Git tag, triggering `.github/workflows/release.yml`.
7. Publish the library crate on crates.io.

Cross toolchains are pinned by digest in `Cross.toml`, rather than floating
`:main`. `CARGO_TARGET_DIR=target/cross` isolates old-sysroot build scripts from
native release-test artifacts in `target`; sharing these caches can produce
host-glibc binaries that fail inside the cross container. The Docker runtime
remains Debian bookworm. Do not fix loader errors simply by skipping the smoke
check or sharing the native cache again.

**RCI completion is not GitHub workflow completion.** Follow the tag's GitHub
release workflow through both macOS/Linux assets and Homebrew-formula update.

## Runner prerequisites

- Rust, both GNU targets, cross, cargo-audit, just, Docker/buildx, reprepro, GPG.
- Sibling `/work/velor-apt` checkout; `APT_REPO` can override it. Both checkouts
  need Git identity for APT commits and annotated source tags.
- Existing runner Git, Docker and Cargo publication credentials in its home.
- Velor APT private signing key in the runner's GPG keyring.
- Bucket-scoped R2 credentials for the APT publisher at its expected path,
  `~/work/x.ssh/cf-velor-small-dawn.json` (0600). This is the narrow existing
  `velor` rclone identity, **not** the Cloudflare management token.
- APT Python venv: `~/.local/share/velor-apt/venv` with repository requirements.

The APT publisher validates InRelease against the expected signing fingerprint,
only uploads public files, sends packages before metadata, verifies uploads,
and never uploads conf/, db/, or private keys.

## Rehearsal and rollback

For an explicitly approved rehearsal, temporarily deploy the same job with
`SKIP_CARGO_PUBLISH=1 just do-release` in its release phase. The checked-in job
must remain the normal publisher; restore that definition after the rehearsal.
`cargo publish --dry-run -p tabularium` can validate crate packaging separately.
A crates.io version **cannot be reused**, even after yanking; never publish it
as part of a reversible rehearsal.

Before a rehearsal, record APT local/remote HEAD, archive the full APT repo
state (including reprepro databases), snapshot bucket keys and metadata bodies,
save Docker latest's exact digest, and save the Homebrew formula. Ensure the new
version/tag does not exist. Keep backups private on the runner.

After successful verification:

- Wait for GitHub workflows to reach a terminal state. Delete only the rehearsal
  release and version tag (remote and runner-local). Never delete an older release.
- Reset APT on lab-rci to the recorded commit, restore archived DB/index state,
  and push with an explicit expected `--force-with-lease`. Stop on any concurrent
  changes. Check clean local state, remote HEAD and `reprepro check`.
- Restore bucket metadata byte-for-byte and remove only package objects added by
  this rehearsal; verify signatures, hashes, public indexes and key inventory.
- Restore Docker latest by its saved digest, delete only the rehearsal version
  tag, and verify the digest and missing tag.
- If GitHub updated `Formula/tabularium.rb`, restore the saved formula in a new
  source commit. Do not hard-reset source main or discard unrelated commits.
- Restore the tracked normal RCI job and verify its exported definition has no
  rehearsal skip flag. Keep source version at the intended next release version.

Git reset alone does not roll back bucket or Docker publication. Publication is
not transactional, so preserve receipts/logs and distinguish build success from
runtime and publication verification.
