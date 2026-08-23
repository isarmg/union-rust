#!/usr/bin/env bash
set -euo pipefail

if [[ ${1:-} != --allow-system-changes ]]; then
  echo "usage: $0 --allow-system-changes [PACKAGE.deb]" >&2
  echo "This test installs, starts, removes, and reconfigures unionc on the current system." >&2
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
    -name 'unionc_*_amd64.deb' -print0)
  if (( ${#packages[@]} != 1 )); then
    echo "error: expected exactly one unionc amd64 DEB in $repository_root/dist, found ${#packages[@]}" >&2
    if (( ${#packages[@]} > 0 )); then
      printf '  %s\n' "${packages[@]}" >&2
    fi
    exit 1
  fi
  package=${packages[0]}
fi
[[ -n $package && -f $package ]]
server_version=$(dpkg-deb --field "$package" Version)
[[ $server_version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]

sudo dpkg -i "$package"
test -x /usr/bin/unionc
getent passwd unionc >/dev/null
getent group unionc >/dev/null
test "$(stat -c '%a:%U:%G' /var/lib/unionc)" = "700:unionc:unionc"
test "$(stat -c '%a:%U:%G' /var/lib/unionc-package)" = "700:root:root"
test "$(stat -c '%a:%U:%G' /etc/unionc)" = "755:root:root"
test "$(stat -c '%a:%U:%G:%h' /etc/unionc/unionc.env)" = \
  "640:root:unionc:1"
for marker in managed-user managed-group; do
  marker_path="/var/lib/unionc-package/$marker"
  test "$(sudo stat -c '%a:%U:%G:%h' "$marker_path")" = "600:root:root:1"
  test "$(sudo sed -n 's/^format=//p' "$marker_path")" = "$server_version"
done
sudo test ! -e /var/lib/unionc-package/pending-group
sudo test ! -e /var/lib/unionc-package/pending-user
test "$(sudo sed -n 's/^uid=//p' /var/lib/unionc-package/managed-user)" = \
  "$(id -u unionc)"
test "$(sudo sed -n 's/^primary_gid=//p' /var/lib/unionc-package/managed-user)" = \
  "$(id -g unionc)"
test "$(sudo sed -n 's/^gid=//p' /var/lib/unionc-package/managed-group)" = \
  "$(getent group unionc | cut -d: -f3)"
sudo grep -Fx "UNIONC_PACKAGE_VERSION=$server_version" /etc/unionc/unionc.env
! systemctl is-enabled --quiet unionc.service
! systemctl is-active --quiet unionc.service
sudo test ! -e /var/lib/unionc/unionc.db
sudo test ! -e /var/lib/unionc/.unionc-data-directory

sudo tee /etc/unionc/unionc.env >/dev/null <<EOF
UNIONC_PACKAGE_VERSION=$server_version
UNIONC_ENV=production
UNIONC_SECRET_KEY=MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=
UNIONC_PROXY_SECRET=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
UNIONC_ALLOW_BOOTSTRAP=1
UNIONC_BOOTSTRAP_PASSWORD=release-smoke-password-2026
EOF
sudo chown root:unionc /etc/unionc/unionc.env
sudo chmod 0640 /etc/unionc/unionc.env

wait_ready() {
  for _ in $(seq 1 60); do
    if curl --fail --silent --show-error \
      http://127.0.0.1:8081/api/ready >/dev/null; then
      return 0
    fi
    sleep 1
  done
  sudo journalctl -u unionc.service --no-pager -n 120 || true
  return 1
}
run_maintenance() {
  sudo systemd-run --quiet --wait --pipe --collect \
    --uid=unionc --gid=unionc \
    -p WorkingDirectory=/var/lib/unionc \
    -p Environment=UNIONC_DATA_DIR=/var/lib/unionc \
    -p EnvironmentFile=/etc/unionc/unionc.env \
    /usr/bin/unionc "$@"
}

sudo systemctl enable --now unionc.service
systemctl is-enabled --quiet unionc.service
wait_ready
test "$(sudo stat -c '%a:%U:%G' /var/lib/unionc/unionc.db)" = "600:unionc:unionc"
data_marker=/var/lib/unionc/.unionc-data-directory
test "$(sudo stat -c '%a:%U:%G:%h' "$data_marker")" = "600:unionc:unionc:1"
test "$(sudo cat "$data_marker")" = "unionc-data-directory-v1"
data_marker_identity=$(sudo stat -c '%d:%i' "$data_marker")
sudo sed -i \
  -e '/^UNIONC_ALLOW_BOOTSTRAP=/d' \
  -e '/^UNIONC_BOOTSTRAP_PASSWORD=/d' \
  /etc/unionc/unionc.env

sudo install -d -m 0700 -o unionc -g unionc /var/backups/unionc-release
backup=/var/backups/unionc-release/unionc.db
run_maintenance backup --output "$backup"
test "$(sudo stat -c '%a:%U:%G' "$backup")" = "600:unionc:unionc"
test "$(sudo stat -c '%a:%U:%G' "${backup}.manifest.json")" = "600:unionc:unionc"

sudo systemctl stop unionc.service
run_maintenance restore --input "$backup" --force
run_maintenance integrity-check
pre_restore="$(sudo find /var/lib/unionc -maxdepth 1 \
  -name 'unionc.pre-restore-*.db' -type f -print -quit)"
test -n "$pre_restore"
test "$(sudo stat -c '%a:%U:%G' "$pre_restore")" = "600:unionc:unionc"
test "$(sudo stat -c '%a:%U:%G' "${pre_restore}.manifest.json")" = \
  "600:unionc:unionc"
run_maintenance restore --input "$pre_restore" --force
run_maintenance integrity-check
sudo systemctl start unionc.service
wait_ready

# A notify-aware reinstall must fail before the package manager reports success
# when the retained runtime configuration cannot initialize the Server.
sudo sed -i \
  -e 's/^UNIONC_PROXY_SECRET=.*/UNIONC_PROXY_SECRET=invalid/' \
  /etc/unionc/unionc.env
if sudo dpkg -i "$package"; then
  echo "Server package accepted a reinstall whose service never became ready" >&2
  exit 1
fi
systemctl is-enabled --quiet unionc.service
sudo sed -i \
  -e 's/^UNIONC_PROXY_SECRET=.*/UNIONC_PROXY_SECRET=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef/' \
  /etc/unionc/unionc.env
sudo dpkg --configure unionc
test "$(dpkg-query -W -f='${Version}' unionc)" = \
  "$(dpkg-deb --field "$package" Version)"
systemctl is-enabled --quiet unionc.service
wait_ready
test "$(sudo stat -c '%d:%i' "$data_marker")" = "$data_marker_identity"

sudo dpkg --remove unionc
test ! -e /usr/bin/unionc
sudo test -e /var/lib/unionc/unionc.db
test "$(sudo stat -c '%d:%i' "$data_marker")" = "$data_marker_identity"

# A retained marker from any other version must fail closed. Restore
# it only to let dpkg finish cleaning up this disposable runner.
sudo sed -i "s/^format=.*/format=0.0.0/" \
  /var/lib/unionc-package/managed-user
if sudo dpkg -i "$package"; then
  echo "Server package adopted another version's ownership marker" >&2
  exit 1
fi
sudo test -e /var/lib/unionc/unionc.db
sudo sed -i "s/^format=.*/format=$server_version/" \
  /var/lib/unionc-package/managed-user
sudo dpkg --configure unionc
sudo dpkg --remove unionc
