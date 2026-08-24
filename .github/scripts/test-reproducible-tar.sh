#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
helper="$repo_root/.github/scripts/create-reproducible-tar.sh"
work_dir=$(mktemp -d "${TMPDIR:-/tmp}/unionc-reproducible-tar.XXXXXX")
cleanup() {
  rm -rf -- "$work_dir"
}
trap cleanup EXIT

source_dir="$work_dir/bundle"
mkdir -p "$source_dir/nested"
printf 'root payload\n' > "$source_dir/root.txt"
printf 'nested payload\n' > "$source_dir/nested/value.txt"

touch -t 202001020304.05 \
  "$source_dir" "$source_dir/nested" \
  "$source_dir/root.txt" "$source_dir/nested/value.txt"
bash "$helper" "$source_dir" "$work_dir/first.tar.gz" 1700000000

touch -t 203012312359.58 \
  "$source_dir" "$source_dir/nested" \
  "$source_dir/root.txt" "$source_dir/nested/value.txt"
bash "$helper" "$source_dir" "$work_dir/second.tar.gz" 1700000000

cmp "$work_dir/first.tar.gz" "$work_dir/second.tar.gz"

expected_listing=$'bundle/\nbundle/nested/\nbundle/nested/value.txt\nbundle/root.txt'
actual_listing=$(tar -tzf "$work_dir/first.tar.gz")
if [[ "$actual_listing" != "$expected_listing" ]]; then
  printf 'unexpected archive contents:\n%s\n' "$actual_listing" >&2
  exit 1
fi

mkdir "$work_dir/extracted"
tar -xzf "$work_dir/first.tar.gz" -C "$work_dir/extracted"
cmp "$source_dir/root.txt" "$work_dir/extracted/bundle/root.txt"
cmp "$source_dir/nested/value.txt" "$work_dir/extracted/bundle/nested/value.txt"

echo "reproducible tar helper tests passed"
