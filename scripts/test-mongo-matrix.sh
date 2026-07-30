#!/usr/bin/env bash
#
# Run the `mongo`-gated test suite once per supported MongoDB server version.
#
# This is the local equivalent of the `integration` matrix in
# .github/workflows/ci.yml. It starts a throwaway container per version on a
# free port, runs the suite against it, and tears it down — so a failure is
# always attributable to a specific server release.
#
#   ./scripts/test-mongo-matrix.sh              # every supported version
#   ./scripts/test-mongo-matrix.sh 8.0          # just one
#   MONGO_VERSIONS="7.0 8.0" ./scripts/test-mongo-matrix.sh
#
# The version list must stay in sync with SUPPORTED_SERVER_VERSIONS in
# tests/support/mod.rs — `server_under_test_is_a_supported_release` fails if a
# version is exercised here without being declared there.

set -euo pipefail

# Keep in sync with SUPPORTED_SERVER_VERSIONS (tests/support/mod.rs) and the
# CI matrix (.github/workflows/ci.yml).
DEFAULT_VERSIONS="6.0 7.0 8.0"
VERSIONS="${*:-${MONGO_VERSIONS:-$DEFAULT_VERSIONS}}"

PORT="${LEAFMASK_MATRIX_PORT:-27117}"
CONTAINER_PREFIX="leafmask-matrix"

command -v docker >/dev/null 2>&1 || {
  echo "error: docker is required to run the MongoDB version matrix" >&2
  exit 1
}

cleanup() {
  docker rm -f "${CONTAINER_PREFIX}-${CURRENT_VERSION:-none}" >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

failed=()

for version in $VERSIONS; do
  CURRENT_VERSION="$version"
  container="${CONTAINER_PREFIX}-${version}"

  echo
  echo "==> MongoDB ${version}"
  docker rm -f "$container" >/dev/null 2>&1 || true
  docker run -d --name "$container" -p "${PORT}:27017" "mongo:${version}" >/dev/null

  # Wait for the server to accept connections rather than sleeping a fixed
  # amount: image pull and startup time vary widely between versions.
  ready=0
  for _ in $(seq 1 60); do
    if docker exec "$container" mongosh --quiet --eval 'db.adminCommand("ping")' \
        >/dev/null 2>&1; then
      ready=1
      break
    fi
    sleep 1
  done
  if [ "$ready" -ne 1 ]; then
    echo "error: MongoDB ${version} did not become ready in 60s" >&2
    docker logs "$container" 2>&1 | tail -20 >&2
    docker rm -f "$container" >/dev/null 2>&1 || true
    failed+=("$version (startup)")
    continue
  fi

  if LEAFMASK_MONGO_URI="mongodb://localhost:${PORT}" \
      cargo test --locked --features mongo; then
    echo "==> MongoDB ${version}: PASS"
  else
    echo "==> MongoDB ${version}: FAIL" >&2
    failed+=("$version")
  fi

  docker rm -f "$container" >/dev/null 2>&1 || true
done

echo
if [ ${#failed[@]} -eq 0 ]; then
  echo "All MongoDB versions passed: ${VERSIONS}"
else
  echo "Failed on: ${failed[*]}" >&2
  exit 1
fi
