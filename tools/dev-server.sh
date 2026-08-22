#!/bin/sh
set -eu

if [ "$(uname -s)" != Linux ]; then
  echo "UnionC Server only supports Linux; use Linux or WSL for local Server development." >&2
  exit 1
fi

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
repository_root=$(CDPATH= cd -- "$script_dir/.." && pwd -P)
data_dir="$repository_root/.runtime/server"

mkdir -p -- "$data_dir"
export UNIONC_DATA_DIR="$data_dir"
cd "$repository_root"
exec cargo run --manifest-path "$repository_root/Cargo.toml" -p unionc -- "$@"
