#!/usr/bin/env bash
set -euo pipefail
umask 077

: "${OSS_BUCKET:?OSS_BUCKET is required}"
: "${OSS_ENDPOINT:?OSS_ENDPOINT is required}"
: "${OSS_ACCESS_KEY_ID:?OSS_ACCESS_KEY_ID is required}"
: "${OSS_ACCESS_KEY_SECRET:?OSS_ACCESS_KEY_SECRET is required}"
: "${OSS_PREFIX:?OSS_PREFIX is required (bucket object prefix)}"
: "${DOMESTIC_MANIFEST:?DOMESTIC_MANIFEST is required}"
: "${VERSION:?VERSION is required}"

ossutil="${RUNNER_TEMP:-/tmp}/ossutil"
curl -fsSL https://gosspublic.alicdn.com/ossutil/1.7.19/ossutil64 -o "$ossutil"
chmod 700 "$ossutil"
config="${RUNNER_TEMP:-/tmp}/ossutilconfig"
trap 'rm -f "$config" "$ossutil"' EXIT
"$ossutil" config -e "$OSS_ENDPOINT" -i "$OSS_ACCESS_KEY_ID" -k "$OSS_ACCESS_KEY_SECRET" -L CH -c "$config"
base="oss://${OSS_BUCKET%/}/${OSS_PREFIX#/}"
version_dir="$base/v${VERSION#v}"
find dist -maxdepth 1 -type f ! -name latest.json -print0 | while IFS= read -r -d '' file; do
  case "$(basename "$file")" in
    latest-domestic.json) ;;
    peercarry-*.tar.gz|peercarry-*.zip) "$ossutil" cp "$file" "$version_dir/$(basename "$file")" -f -c "$config" ;;
    *) "$ossutil" cp "$file" "$version_dir/raw/$(basename "$file")" -f -c "$config" ;;
  esac
done
"$ossutil" cp "$DOMESTIC_MANIFEST" "$version_dir/latest.json" -f -c "$config"
"$ossutil" cp "$DOMESTIC_MANIFEST" "$base/latest.json" -f -c "$config"
