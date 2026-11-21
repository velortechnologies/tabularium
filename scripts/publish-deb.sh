#!/usr/bin/env bash
set -euo pipefail
version=${1:?version required}
source_dir=$(cd "$(dirname "$0")/.." && pwd)
apt_repo=${APT_REPO:-$(dirname "$source_dir")/velor-apt}
exec 9>"${apt_repo}.release.lock"
flock -n 9
cd "$apt_repo"
test -z "$(git status --porcelain)" || { echo 'APT checkout is dirty'; exit 1; }
git pull --ff-only
for arch in arm64 amd64; do
    for package in cli server; do
        reprepro -b repo includedeb stable "$source_dir/make-deb/dist/tabularium-${package}_${version}_${arch}.deb"
    done
done
# auto-add.sh also commits with a generic message. Stage all generated changes
# directly so packages, indexes and reprepro state share the requested message.
git add -- repo
if ! git diff --cached --quiet; then
    git commit -m "tb $version"
fi
git push origin HEAD
just pub
