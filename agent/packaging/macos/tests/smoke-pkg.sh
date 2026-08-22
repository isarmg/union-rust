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
  packages=()
  while IFS= read -r -d '' candidate; do
    packages+=("$candidate")
  done < <(find "$repository_root/dist" -maxdepth 1 -type f \
    -name 'unionc-agent-*.pkg' -print0)
  if (( ${#packages[@]} != 1 )); then
    echo "expected exactly one UnionC Agent pkg in $repository_root/dist; found ${#packages[@]}" >&2
    printf '  %s\n' "${packages[@]}" >&2
    exit 1
  fi
  package=${packages[0]}
fi
[[ -n $package && -f $package ]]

assert_no_extended_acl() {
  local path=$1
  local listing first_line permissions line_count
  listing=$(sudo env LC_ALL=C ls -lde "$path")
  first_line=${listing%%$'\n'*}
  permissions=${first_line%% *}
  [[ -n $permissions && $permissions != *+ ]] || {
    echo "unexpected extended ACL on $path" >&2
    exit 1
  }
  line_count=$(printf '%s\n' "$listing" | wc -l | tr -d '[:space:]')
  [[ $line_count == 1 ]] || {
    echo "unexpected ACL entries on $path" >&2
    exit 1
  }
}

assert_ownership_proof() {
  [[ $(sudo stat -f '%u:%g:%Mp:%Lp' /var/db/unionc-agent) == 0:0:0:700 ]]
  [[ $(sudo stat -f '%u:%g:%Mp:%Lp' /var/db/unionc-agent/account-ownership) == 0:0:0:600 ]]
  assert_no_extended_acl /var/db/unionc-agent
  assert_no_extended_acl /var/db/unionc-agent/account-ownership
}

sudo installer -pkg "$package" -target /
sudo launchctl print system/com.unionc.agent >/dev/null
assert_ownership_proof
sudo touch '/Library/Application Support/UnionC Agent/release-lifecycle-marker'

sudo installer -pkg "$package" -target /
sudo launchctl print system/com.unionc.agent >/dev/null
assert_ownership_proof
sudo /usr/local/share/unionc-agent/uninstall.sh
[[ ! -e /usr/local/libexec/unionc-agent ]]
[[ -e '/Library/Application Support/UnionC Agent/release-lifecycle-marker' ]]
assert_ownership_proof

sudo installer -pkg "$package" -target /
assert_ownership_proof
sudo /usr/local/share/unionc-agent/uninstall.sh --purge --yes
[[ ! -e '/Library/Application Support/UnionC Agent' ]]
[[ ! -e /var/db/unionc-agent ]]
! dscl . -read /Users/_unioncagent >/dev/null 2>&1
! dscl . -read /Groups/_unioncagent >/dev/null 2>&1
! pkgutil --pkg-info com.unionc.agent >/dev/null 2>&1
