#!/usr/bin/env bash
# Records one demo GIF: tape/<name>.tape -> assets/<name>.gif.
#
#     tape/record.sh demo
#
# The recording runs in a container that holds the iktinos binary, VHS and
# nothing of this machine: only `tape/` (read-only) and `assets/` are
# mounted, there is no network, and the hostname is fixed. The sessions it
# shows are the invented ones tape/demo-fixture.py writes inside the
# container. Do not run `vhs` on a tape directly: on the host, iktinos
# would find the real sessions.
set -euo pipefail

name=${1:?usage: tape/record.sh <name>}
root=$(cd "$(dirname "$0")/.." && pwd)
[ -f "$root/tape/$name.tape" ] || { echo "no tape/$name.tape" >&2; exit 1; }

docker build --quiet --file "$root/tape/Dockerfile" --tag iktinos-demo "$root" >/dev/null
mkdir -p "$root/assets"
docker run --rm \
  --network none \
  --hostname demo \
  --volume "$root/tape:/demo/tape:ro" \
  --volume "$root/assets:/demo/assets" \
  iktinos-demo "tape/$name.tape"
