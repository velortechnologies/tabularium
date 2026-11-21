# Tabularium — run with: just test, just fmt, etc.

VERSION := `grep ^version Cargo.toml | cut -d'"' -f2`

default:
    just --list

# Vite → ui/dist, embedded by tabularium-server via include_dir!. Run before any server compile.
build-ui:
    cd ui && npm ci && npm run build

# Uses `config.toml` if present, else `config.toml.example` (fresh clones work without copying).
run: build-ui
    bash -ec 'cfg=config.toml; [[ -f "$cfg" ]] || cfg=config.toml.example; exec cargo run -p tabularium-server --release -- --config "$cfg"'

tb *args:
    cargo run -p tabularium-cli --release -- {{args}}

fmt:
    cargo fmt --all

# Multi-arch publish needs a buildx builder; prefer docker-container driver (avoids flaky default).
prepare-docker:
    bash -ec '\
      set -euo pipefail; \
      if docker buildx inspect tabularium-multi >/dev/null 2>&1; then \
        docker buildx use tabularium-multi; \
      else \
        docker buildx create --name tabularium-multi --driver docker-container --use; \
      fi; \
      docker buildx inspect --bootstrap \
    '

# Reuse the exact cross-built DEB binaries in the Docker runtime check.
stage-docker: deb
    mkdir -p docker/_build/amd64 docker/_build/arm64
    cp target/cross/x86_64-unknown-linux-gnu/release/tabularium-server docker/_build/amd64/tabularium-server
    cp target/cross/x86_64-unknown-linux-gnu/release/tb docker/_build/amd64/tb
    cp target/cross/aarch64-unknown-linux-gnu/release/tabularium-server docker/_build/arm64/tabularium-server
    cp target/cross/aarch64-unknown-linux-gnu/release/tb docker/_build/arm64/tb

# Dockerfile executes both binaries on each platform before any public writes.
check-runtime: stage-docker prepare-docker
    docker buildx build --platform "${PLATFORMS:-linux/amd64,linux/arm64}" -f Dockerfile.publish .

pub-docker: check-runtime
    bash -ec ' \
      IMAGE="${IMAGE:-velortech/tabularium}"; \
      VERSION="{{VERSION}}"; \
      TAG="${TAG:-}"; \
      PLATFORMS="${PLATFORMS:-linux/amd64,linux/arm64}"; \
      if [[ -z "${TAG}" ]]; then \
        docker buildx build --platform "${PLATFORMS}" -f Dockerfile.publish -t "${IMAGE}:${VERSION}" -t "${IMAGE}:latest" --push .; \
      else \
        docker buildx build --platform "${PLATFORMS}" -f Dockerfile.publish -t "${IMAGE}:${TAG}" --push .; \
      fi; \
    '

tag:
    git tag -a v{{VERSION}} -m v{{VERSION}}
    git push origin --tags

# Run the release through RCI; vulnerability findings block publication.
release:
    rci x tabularium

# CI entry point; never call the RCI launcher recursively.
do-release: release-preflight audit check-runtime pub-deb pub-docker tag publush-cargo-crate

release-preflight:
    bash scripts/release-preflight.sh

# Report informational warnings without blocking; vulnerabilities remain release blockers.
audit:
    cargo audit

publush-cargo-crate:
    bash -ec 'if [[ "${SKIP_CARGO_PUBLISH:-0}" == 1 ]]; then echo "Rehearsal: crates.io publication explicitly skipped"; else cargo publish -p tabularium; fi'

# Patch-bump [workspace.package] version, then cargo check --workspace (refresh lock/metadata).
bump:
    python3 scripts/workspace_version.py bump

# Set workspace version to X.Y.Z (digits only), then cargo check --workspace.
set-version version:
    python3 scripts/workspace_version.py set "{{version}}"

check: build-ui
    cargo check --workspace

# Library only: storage stack with the optional embeddings HTTP client (matches server dependency shape).
check-tabularium-db:
    cargo check -p tabularium --no-default-features --features db

# Minimal crate surface (`error`, `validation`, `resource_path` only).
check-tabularium-core:
    cargo check -p tabularium --no-default-features

# Release-mode tests (integration workflow / CI parity).
test: build-ui
    cargo test --workspace --release

# Fast local iteration (debug artifacts).
test-dev: build-ui
    cargo test --workspace

# Local release-only (no pytest / no server).
test-ci: build-ui
    cargo test --workspace --release

clippy: build-ui
    clippy

# Remote integration workflow: rsync → /opt/${CI_PROJECT:-tabularium}, release build, ephemeral server (logs in /tmp), pytest, `just test`, trap kills server.
# Override: CI_HOST CI_PROJECT CI_LISTEN. See scripts/ci-test.sh.
ci-test:
    bash scripts/ci-test.sh

# --- Debian packages (.deb) ---
# Build needs https://github.com/cross-rs/cross (and Docker) when not on the target Linux arch.
# Packing needs `dpkg-deb` (run on Debian/Ubuntu or a container even if cross ran elsewhere).
deb-amd64: build-ui
    env CARGO_TARGET_DIR=target/cross cross build --target x86_64-unknown-linux-gnu --release --features mcp -p tabularium-server -p tabularium-cli
    env TARGET_DIR=target/cross RUST_TARGET=x86_64-unknown-linux-gnu DEB_ARCH=amd64 bash make-deb/build.sh

deb-arm64: build-ui
    env CARGO_TARGET_DIR=target/cross cross build --target aarch64-unknown-linux-gnu --release --features mcp -p tabularium-server -p tabularium-cli
    env TARGET_DIR=target/cross RUST_TARGET=aarch64-unknown-linux-gnu DEB_ARCH=arm64 bash make-deb/build.sh

# Both architectures (sequential).
deb: deb-amd64 deb-arm64

# Publish built .debs, commit all APT changes and push before bucket publication.
# APT_REPO defaults to the sibling velor-apt checkout.
pub-deb:
    bash scripts/publish-deb.sh "{{VERSION}}"

# On x86_64 Linux: same artifacts without Docker/cross.
deb-native-amd64: build-ui
    cargo build --target x86_64-unknown-linux-gnu --release --features mcp -p tabularium-server -p tabularium-cli
    env TARGET_DIR=target RUST_TARGET=x86_64-unknown-linux-gnu DEB_ARCH=amd64 bash make-deb/build.sh

build-brew: build-ui
    mkdir -p .brew-dist
    cargo build --release -p tabularium-server -p tabularium-cli
    cp target/release/tabularium-server .brew-dist/tabularium-server
    cp target/release/tb .brew-dist/tb

brew-install: build-brew
    bash scripts/brew-install.sh
