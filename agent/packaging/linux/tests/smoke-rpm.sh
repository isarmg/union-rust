#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repository_root=$(cd -- "$script_dir/../../../.." && pwd)
artifact_dir=${1:-"$repository_root/dist"}
[[ -d $artifact_dir ]]
artifact_dir=$(cd -- "$artifact_dir" && pwd)

docker run --rm \
  --volume "$artifact_dir:/artifacts:ro" \
  fedora:44 /bin/bash -euxo pipefail -c '
    packages=()
    while IFS= read -r -d "" candidate; do
      packages+=("$candidate")
    done < <(find /artifacts -maxdepth 1 -type f \
      -name "unionc-agent-*.x86_64.rpm" -print0)
    if (( ${#packages[@]} != 1 )); then
      echo "error: expected exactly one unionc-agent x86_64 RPM in /artifacts, found ${#packages[@]}" >&2
      if (( ${#packages[@]} > 0 )); then
        printf "  %s\n" "${packages[@]}" >&2
      fi
      exit 1
    fi
    package=${packages[0]}
    dnf install -y "$package"
    test -x /usr/bin/unionc-agent
    touch /var/lib/unionc-agent/release-lifecycle-marker

    dnf remove -y unionc-agent
    test ! -e /usr/bin/unionc-agent
    test -e /var/lib/unionc-agent/release-lifecycle-marker
    test -e /etc/unionc-agent/config.json

    dnf install -y "$package"
    /usr/sbin/unionc-agent-purge --yes
    dnf remove -y unionc-agent
    test ! -e /var/lib/unionc-agent
    test ! -e /etc/unionc-agent
    ! getent passwd unionc-agent
    ! getent group unionc-agent
  '
