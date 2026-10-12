#!/usr/bin/env bash
# Builds Signalsmith Stretch's command-line tool into eval/cache/bin, because
# no distribution packages it. Needs git and a C++11 compiler. Both repositories
# are cloned at the revisions below, so the reference stays the same until
# someone changes them on purpose.
set -euo pipefail

STRETCH_REV=${STRETCH_REV:-a670068d9aeb64913331d5cc29337b19a457a7df}
LINEAR_REV=${LINEAR_REV:-146b26f91b48835d665fa0498d64d9617818d27a}

here=$(cd "$(dirname "$0")" && pwd)
src="$here/cache/src"
out="$here/cache/bin"
mkdir -p "$src" "$out"

clone() { # name url rev
    if [ ! -d "$src/$1/.git" ]; then
        git clone --quiet "$2" "$src/$1"
    fi
    git -C "$src/$1" fetch --quiet origin "$3"
    git -C "$src/$1" checkout --quiet --detach FETCH_HEAD
}

clone signalsmith-stretch https://github.com/Signalsmith-Audio/signalsmith-stretch.git "$STRETCH_REV"
clone signalsmith-linear https://github.com/Signalsmith-Audio/linear.git "$LINEAR_REV"

# The command-line tool's WAV and argument helpers are a submodule.
git -C "$src/signalsmith-stretch" submodule update --quiet --init cmd/util

# The tool includes "signalsmith-stretch/..." and the header includes
# "signalsmith-linear/...", so give the compiler a folder holding both names.
inc="$src/include"
mkdir -p "$inc"
ln -sfn "$src/signalsmith-stretch" "$inc/signalsmith-stretch"
ln -sfn "$src/signalsmith-linear" "$inc/signalsmith-linear"

"${CXX:-g++}" -std=c++11 -O3 -I "$inc" \
    "$src/signalsmith-stretch/cmd/main.cpp" -o "$out/signalsmith-stretch"

echo "built $out/signalsmith-stretch ($(git -C "$src/signalsmith-stretch" rev-parse --short HEAD), linear $(git -C "$src/signalsmith-linear" rev-parse --short HEAD))"
