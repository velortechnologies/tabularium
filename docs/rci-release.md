# RCI release

`just release` launches `rci x tabularium`. RCI runs UI build and release-mode
workspace tests, then `just do-release`: duplicate-tag preflight, dependency
audit, both Debian architectures, APT commit/push and bucket publication,
multi-platform Docker publication, Git tag (triggers GitHub release workflow),
and crates.io publication.

Job definition: `ci/tabularium.yml`. Deploy via `rci job deploy tabularium < ci/tabularium.yml`.
Runner: lab-rci, user rci. Node 22 is installed/selected through existing NVM.
Requires Rust/cross/cargo-audit, Docker/buildx, reprepro, GPG and just on PATH;
sibling `velor-apt` checkout (override APT_REPO); Git, Docker, Cargo and R2
credentials in the runner's home; APT publisher's Python venv.

APT commits use `tb VERSION` and include the full generated `repo` state.
Publication is not transactional. Record the previous APT commit and registry
digests before a rehearsal. Deleting a GH release and resetting APT Git alone
is NOT full rollback: restore bucket indexes and Docker tags too. Crates.io
versions cannot be reused after publication/yanking. Existing release tags are
rejected before builds/publication. Do not delete a pre-existing release to test.
