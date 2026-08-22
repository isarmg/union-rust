#!/bin/sh
set -eu

if [ "$(uname -s)" != Linux ]; then
  echo "UnionC Server only supports Linux; use Linux or WSL for local Server development." >&2
  exit 1
fi

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
repository_root=$(CDPATH= cd -- "$script_dir/.." && pwd -P)
data_dir="$repository_root/.runtime/server"

umask 077
mkdir -p -- "$data_dir"
data_mode=$(stat -c '%a' -- "$data_dir")
if [ "$data_mode" != 700 ]; then
  echo "UnionC data directory must already be mode 0700: $data_dir (found $data_mode)." >&2
  echo "After confirming that this is the dedicated UnionC directory, run: chmod 0700 -- '$data_dir'" >&2
  exit 1
fi
export UNIONC_DATA_DIR="$data_dir"
cd "$repository_root"
exec cargo run --manifest-path "$repository_root/Cargo.toml" -p unionc -- "$@"
