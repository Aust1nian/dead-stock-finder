#!/usr/bin/env bash
# Runs a command in the Linux development image with the repo at /repo.
# Usage: tools/docker/run.sh "<command>"
set -euo pipefail
repo="$(cd "$(dirname "$0")/../.." && pwd)"
docker image inspect dsf-linux-dev >/dev/null 2>&1 || docker build -t dsf-linux-dev -f "$repo/tools/docker/linux-dev.Dockerfile" "$repo/tools/docker"
exec docker run --rm -i \
  -u "$(id -u):$(id -g)" \
  -e HOME=/tmp/home -e CARGO_TARGET_DIR=/repo/target/linux-docker \
  -e CARGO_HOME=/repo/target/linux-docker-cargo-home \
  -v "$repo":/repo -w /repo \
  dsf-linux-dev bash -lc "mkdir -p \$HOME && export PATH=/opt/cargo/bin:\$PATH && $*"
