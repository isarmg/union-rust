#!/bin/sh
set -eu

service_name=unionc-agent.service
package_version=0.3.2
account_state_dir=/var/lib/unionc-agent-package
rpm_config_backup="$account_state_dir/config.json.remove-backup"

systemd_is_running() {
  [ -d /run/systemd/system ]
}

die() {
  echo "unionc-agent preremove: $*" >&2
  exit 1
}

stop_for_current_reinstall() {
  if systemd_is_running; then
    command -v systemctl >/dev/null 2>&1 || die "systemd is running but systemctl is unavailable"
    systemctl stop "$service_name"
  fi
}

disable_for_remove() {
  if systemd_is_running; then
    command -v systemctl >/dev/null 2>&1 || {
      echo "unionc-agent preremove: systemd is running but systemctl is unavailable" >&2
      exit 1
    }
    systemctl disable --now "$service_name"
  fi
}

# Debian uses the literal `upgrade <new-version>` ABI even when reinstalling
# the exact same package. Accept only 0.3.2. RPM uses a positive remaining
# instance count for replacement; the new postinstall has already validated
# the exact 0.3.2 ownership markers before the pre-remove scriptlet can run.
case "${1:-}" in
  upgrade)
    [ "$#" -eq 2 ] && [ "$2" = "$package_version" ] ||
      die "cross-version replacement is unsupported; purge before installing another version"
    stop_for_current_reinstall
    ;;
  *[!0-9]*|'')
    disable_for_remove
    ;;
  *[1-9]*)
    :
    ;;
  *)
    # RPM has no purge transaction and may remove an unchanged noreplace
    # config. Save it for ordinary removal only.
    if [ -f /etc/unionc-agent/config.json ]; then
      install -d -m 0700 -o root -g root "$account_state_dir"
      cp -p /etc/unionc-agent/config.json "$rpm_config_backup"
      chmod 0600 "$rpm_config_backup"
    fi
    disable_for_remove
    ;;
esac
