#!/usr/bin/env bash
set -euo pipefail

if [[ ${1:-} != --allow-system-changes ]]; then
  echo "usage: $0 --allow-system-changes [PACKAGE.pkg]" >&2
  echo "This test installs and purges UnionC Agent on the current system." >&2
  exit 2
fi
shift

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repository_root=$(cd -- "$script_dir/../../../.." && pwd)
package=${1:-}
if [[ -z $package ]]; then
  package=$(find "$repository_root/dist" -maxdepth 1 \
    -name 'unionc-agent-*.pkg' -print -quit)
fi
[[ -n $package && -f $package ]]

sudo installer -pkg "$package" -target /
sudo launchctl print system/com.unionc.agent >/dev/null
sudo touch '/Library/Application Support/UnionC Agent/release-lifecycle-marker'

sudo installer -pkg "$package" -target /
sudo launchctl print system/com.unionc.agent >/dev/null
sudo /usr/local/share/unionc-agent/uninstall.sh
[[ ! -e /usr/local/libexec/unionc-agent ]]
[[ -e '/Library/Application Support/UnionC Agent/release-lifecycle-marker' ]]

sudo installer -pkg "$package" -target /
sudo /usr/local/share/unionc-agent/uninstall.sh --purge --yes
[[ ! -e '/Library/Application Support/UnionC Agent' ]]
! dscl . -read /Users/_unioncagent >/dev/null 2>&1
! dscl . -read /Groups/_unioncagent >/dev/null 2>&1
! pkgutil --pkg-info com.unionc.agent >/dev/null 2>&1
