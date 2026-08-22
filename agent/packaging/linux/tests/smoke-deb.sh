#!/usr/bin/env bash
set -euo pipefail

if [[ ${1:-} != --allow-system-changes ]]; then
  echo "usage: $0 --allow-system-changes [PACKAGE.deb]" >&2
  echo "This test installs and purges unionc-agent on the current system." >&2
  exit 2
fi
shift

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repository_root=$(cd -- "$script_dir/../../../.." && pwd)
package=${1:-}
if [[ -z $package ]]; then
  package=$(find "$repository_root/dist" -maxdepth 1 \
    -name 'unionc-agent_*_amd64.deb' -print -quit)
fi
[[ -n $package && -f $package ]]

sudo dpkg -i "$package"
systemctl is-enabled --quiet unionc-agent.service
systemctl is-active --quiet unionc-agent.service
sudo touch /var/lib/unionc-agent/release-lifecycle-marker

sudo dpkg --remove unionc-agent
[[ ! -e /usr/bin/unionc-agent ]]
[[ -e /var/lib/unionc-agent/release-lifecycle-marker ]]
[[ -e /etc/unionc-agent/config.json ]]

sudo dpkg -i "$package"
systemctl is-active --quiet unionc-agent.service
sudo dpkg --purge unionc-agent
[[ ! -e /var/lib/unionc-agent ]]
[[ ! -e /etc/unionc-agent ]]
! getent passwd unionc-agent
! getent group unionc-agent
