#!/bin/sh
# Packs the Mac files of a release for the app's own update (src/update.rs)
# and adds them to the release's update.json, which the Windows job wrote.
#
#   .github/scripts/package_update_mac.sh <folder holding the built iai> <update.json> <output folder>
#
# The build is the one with the Pro part (raw-dll): `iai` and, beside it or in
# deps, `libiai_raw.dylib`. Into the output folder go `iai-mac.gz`,
# `libiai_raw.dylib.gz` and update.json with two more entries, each named as
# the file is installed in iAi.app/Contents/MacOS, with the bytes to download
# and the SHA-256 of the file once unpacked. Both builds must be of the same
# commit: a Mac app takes the Mac files only of the build update.json names.
set -eu

from="$1"
feed="$2"
out="$3"

raw=""
for place in "$from/libiai_raw.dylib" "$from/deps/libiai_raw.dylib"; do
    if [ -f "$place" ]; then
        raw="$place"
        break
    fi
done
if [ -z "$raw" ]; then
    echo "no libiai_raw.dylib in $from or $from/deps: build with the Pro part" >&2
    exit 1
fi

built="$("$from/iai" --build-info | python3 -c 'import json,sys; print(json.load(sys.stdin)["commit"])')"
published="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["commit"])' "$feed")"
if [ "$built" != "$published" ]; then
    echo "the Mac build is of $built, update.json of $published: not the same build" >&2
    exit 1
fi

mkdir -p "$out"
gzip -9 -c "$from/iai" > "$out/iai-mac.gz"
gzip -9 -c "$raw" > "$out/libiai_raw.dylib.gz"

python3 - "$feed" "$out" "$from/iai" "$raw" <<'PY'
import hashlib, json, os, sys

feed, out, exe, library = sys.argv[1:5]
release = json.load(open(feed, encoding="utf-8"))

def entry(name, asset, installed):
    with open(installed, "rb") as f:
        sha256 = hashlib.sha256(f.read()).hexdigest()
    return {
        "name": name,
        "asset": asset,
        "bytes": os.path.getsize(os.path.join(out, asset)),
        "sha256": sha256,
    }

mac = [
    entry("iai", "iai-mac.gz", exe),
    entry("libiai_raw.dylib", "libiai_raw.dylib.gz", library),
]
names = {file["name"] for file in mac}
release["files"] = [f for f in release["files"] if f["name"] not in names] + mac
with open(os.path.join(out, "update.json"), "w", encoding="utf-8") as f:
    json.dump(release, f, ensure_ascii=False, indent=2)
print("packed", ", ".join(f"{e['name']} ({e['bytes']} bytes)" for e in mac))
PY
