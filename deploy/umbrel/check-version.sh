#!/usr/bin/env bash
# The Umbrel app version, the image tag it pins, and the workspace version move together:
# one Alamo release is one Umbrel app release. This fails CI when they drift, and once
# the release image exists on ghcr.io it also checks the pinned digest is that image's.
set -euo pipefail
cd "$(dirname "$0")/../.."

cargo=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
umbrel=$(sed -n 's/^version: "\(.*\)"/\1/p' deploy/umbrel/longstreet-alamo/umbrel-app.yml)
image=$(sed -n 's/^ *image: *//p' deploy/umbrel/longstreet-alamo/docker-compose.yml)
repo=${image%%:*}
tag=${image#*:}; tag=${tag%%@*}
digest=${image#*@}

fail() { echo "error: $*" >&2; exit 1; }

echo "Cargo.toml $cargo · umbrel-app.yml $umbrel · image $repo:$tag@$digest"
[ "$cargo" = "$umbrel" ] || fail "umbrel-app.yml version $umbrel != Cargo.toml version $cargo"
[ "$tag" = "$umbrel" ] || fail "docker-compose.yml pins image tag $tag, not $umbrel"
[[ "$digest" == sha256:* ]] || fail "docker-compose.yml image is not pinned by digest"

# ghcr.io serves public images to anonymous pulls with a scoped token.
path=${repo#ghcr.io/}
token=$(curl -fsS "https://ghcr.io/token?scope=repository:${path}:pull" | sed -n 's/.*"token":"\([^"]*\)".*/\1/p')
actual=$(curl -fsS -o /dev/null -D - \
    -H "Authorization: Bearer $token" \
    -H "Accept: application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json" \
    "https://ghcr.io/v2/${path}/manifests/${tag}" 2>/dev/null \
    | tr -d '\r' | sed -n 's/^docker-content-digest: *//Ip') || true

if [ -z "$actual" ]; then
    echo "image $repo:$tag is not published yet; digest unchecked (tag v$cargo to build it)"
    exit 0
fi
[ "$actual" = "$digest" ] || fail "digest for $repo:$tag is $actual, docker-compose.yml pins $digest"
echo "ok"
