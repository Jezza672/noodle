#!/usr/bin/env bash
# Downloads the CC0 clips listed in eval/clips.csv into eval/cache/clips and
# checks each against its pinned SHA-256, so the borrowed-truth scores are
# always about the same audio. Nothing downloaded is committed.
#
#   eval/fetch.sh            fetch every clip that is not already here
#   eval/fetch.sh NAME...    only those
#   eval/fetch.sh --pin      for rows with no checksum yet: fetch, then print
#                            the checksum to paste into clips.csv (it is
#                            trust on first use, so look at the file first)
#
# clips.csv columns: name,kind,url,sha256,license,source. `kind` is speech,
# instrument or mix; `license` must be CC0 (or public domain) and `source`
# says where the page for the clip is. No commas inside a field.
#
# Any other audio can be dropped straight into eval/local (also ignored by
# git); `noodle-eval borrowed` reads both folders.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
out="$here/cache/clips"
mkdir -p "$out"

pin=0
want=()
for arg in "$@"; do
    case "$arg" in
        --pin) pin=1 ;;
        -h | --help) sed -n '2,16p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) want+=("$arg") ;;
    esac
done

sha256() {
    if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1
    else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

failed=0
count=0
while IFS=, read -r name kind url sum license source; do
    [ "$name" = name ] && continue
    [ -z "$name" ] && continue
    if [ ${#want[@]} -gt 0 ] && ! printf '%s\n' "${want[@]}" | grep -qx "$name"; then
        continue
    fi
    count=$((count + 1))
    case "$license" in
        CC0* | PD* | "Public domain"*) ;;
        *) echo "$name: license '$license' is not CC0 or public domain; skipped" >&2
           failed=1; continue ;;
    esac
    ext=${url##*.}; ext=${ext%%\?*}
    file="$out/$name.$ext"
    if [ ! -f "$file" ]; then
        echo "fetching $name ($kind)"
        curl -fsSL --retry 3 -o "$file.part" "$url"
        mv "$file.part" "$file"
    fi
    got=$(sha256 "$file")
    if [ -z "$sum" ]; then
        if [ "$pin" = 1 ]; then
            echo "$name sha256 $got"
        else
            echo "$name: no checksum pinned in clips.csv (run with --pin to see it)" >&2
            rm -f "$file"; failed=1
        fi
    elif [ "$got" != "$sum" ]; then
        echo "$name: checksum is $got, clips.csv pins $sum; removed" >&2
        rm -f "$file"; failed=1
    fi
done <"$here/clips.csv"

if [ "$count" = 0 ]; then
    echo "no clips listed in eval/clips.csv (or none match); see the header of this script" >&2
    exit 1
fi
exit "$failed"
