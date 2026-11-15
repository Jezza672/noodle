#!/usr/bin/env bash
# Builds the VST3 (and, on macOS, the Audio Unit) of Noodle: the plugin as a
# static library with cargo, then clap-wrapper through CMake. Run from
# anywhere; output goes to `build/packaging/out/`.
#
#   packaging/build.sh [release|debug]
set -euo pipefail
profile="${1:-release}"
root="$(cd "$(dirname "$0")/.." && pwd)"
cargo_args=(--manifest-path "$root/Cargo.toml" -p noodle-plugin --lib --features wrapper --crate-type staticlib)
dir=debug
if [ "$profile" = release ]; then cargo_args+=(--release); dir=release; fi
cargo rustc "${cargo_args[@]}"
target="${CARGO_TARGET_DIR:-$root/target}/$dir"
lib="$target/libnoodle_plugin.a"
[ -f "$lib" ] || lib="$target/noodle_plugin.lib"
cmake -S "$root/packaging" -B "$root/build/packaging" -DCMAKE_BUILD_TYPE=Release -DNOODLE_PLUGIN_LIB="$lib"
cmake --build "$root/build/packaging" --config Release --target noodle_all
echo "Built: $root/build/packaging/out"
