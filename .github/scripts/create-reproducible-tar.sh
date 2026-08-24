#!/usr/bin/env bash
set -euo pipefail

if (( $# != 3 )); then
  echo "usage: $0 SOURCE_DIR OUTPUT.tar.gz SOURCE_DATE_EPOCH" >&2
  exit 2
fi

source_dir=${1%/}
output=$2
source_date_epoch=$3

if [[ ! -d "$source_dir" ]]; then
  echo "source directory does not exist: $source_dir" >&2
  exit 2
fi
if [[ ! "$source_date_epoch" =~ ^[0-9]+$ ]]; then
  echo "SOURCE_DATE_EPOCH must be a non-negative integer: $source_date_epoch" >&2
  exit 2
fi

source_parent=$(dirname -- "$source_dir")
source_name=$(basename -- "$source_dir")
output_parent=$(dirname -- "$output")
if [[ ! -d "$output_parent" ]]; then
  echo "output directory does not exist: $output_parent" >&2
  exit 2
fi

temporary_output=$(mktemp "${output}.tmp.XXXXXX")
cleanup() {
  rm -f -- "$temporary_output"
}
trap cleanup EXIT

LC_ALL=C TZ=UTC tar \
  --sort=name \
  --mtime="@$source_date_epoch" \
  --owner=0 \
  --group=0 \
  --numeric-owner \
  -C "$source_parent" \
  -cf - \
  "$source_name" \
  | gzip -n > "$temporary_output"

chmod 0644 "$temporary_output"
mv -f -- "$temporary_output" "$output"
trap - EXIT
