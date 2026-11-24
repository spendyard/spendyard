#!/usr/bin/env bash
# Compiles the API's contract (proto/) into the CLI, for `spendyard api`:
# src/cli/api/contract.binpb, its descriptors with their comments and what
# they import, built by buf; and contract.sha256, the digest of the .proto
# files it was built from, which a test holds proto/ to. Run it whenever
# proto/ changes:
#
#   scripts/api-contract.sh
set -euo pipefail

cd "$(dirname "$0")/.."

buf build proto -o src/cli/api/contract.binpb

# The .proto files in byte order of their paths, without carriage returns, as
# the test reads them.
find proto -name '*.proto' | LC_ALL=C sort | xargs cat | tr -d '\r' | shasum -a 256 | awk '{ print $1 }' > src/cli/api/contract.sha256

echo "compiled proto/ into src/cli/api/contract.binpb ($(wc -c < src/cli/api/contract.binpb | tr -d ' ') bytes)"
