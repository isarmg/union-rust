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
    package="$(find /artifacts -maxdepth 1 -name "unionc-agent-*.x86_64.rpm" -print -quit)"
    test -n "$package"
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
