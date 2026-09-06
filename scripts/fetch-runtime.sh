#!/usr/bin/env bash
# Downloads the prebuilt Moonshine Linux runtime that Flow links against.
#
# It goes to $XDG_DATA_HOME/flow rather than into the source tree: it is a
# handful of shared libraries, nothing version control should carry.
#
# Pinned to a release tag on purpose. Upstream is at v0.1.x and moves weekly;
# the header we compile against must match the binary we link.

set -euo pipefail

tag='v0.1.5'
data_home="${XDG_DATA_HOME:-$HOME/.local/share}"
dest="$data_home/flow"
url="https://github.com/moonshine-ai/moonshine/releases/download/$tag/moonshine-voice-linux-x86_64.tar.gz"

mkdir -p "$dest"
archive="$(mktemp -t moonshine-voice-linux-x86_64.XXXXXX.tar.gz)"
trap 'rm -f "$archive"' EXIT

echo "Downloading Moonshine runtime $tag ..."
curl -sL --fail -o "$archive" "$url"

extracted="$dest/moonshine-voice-linux-x86_64"
final="$dest/moonshine"
rm -rf "$extracted" "$final"

echo "Extracting to $final ..."
tar -xzf "$archive" -C "$dest"
mv "$extracted" "$final"

lib="$final/lib/libmoonshine.so"
if [[ ! -f "$lib" ]]; then
    echo "extraction did not produce $lib" >&2
    exit 1
fi

echo "Runtime ready at $final"
echo "Contents:"
du -h "$final"/lib/* | sort -k2
