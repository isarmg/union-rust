#!/bin/sh
set -eu

PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH

service_name=unionc-agent.service
package_version=0.5.0
account_state_dir=/var/lib/unionc-agent-package
state_dir=/var/lib/unionc-agent
config_dir=/etc/unionc-agent
config_path="$config_dir/config.json"
rpm_config_backup="$account_state_dir/config.json.remove-backup"
managed_user_marker="$account_state_dir/managed-user"
managed_group_marker="$account_state_dir/managed-group"
backup_temporary=
recorded_user_uid=
recorded_user_primary_gid=
recorded_group_gid=

systemd_is_running() {
  [ -d /run/systemd/system ]
}

die() {
  echo "unionc-agent preremove: $*" >&2
  exit 1
}

read_path_metadata() {
  metadata_path=$1
  path_metadata=$(stat -c '%u:%g:%a' -- "$metadata_path") ||
    die "cannot read ownership and permissions for $metadata_path"
  case "$path_metadata" in
    *[!0-9:]*) die "$metadata_path has invalid ownership or permission metadata" ;;
    :*|*::*|*:) die "$metadata_path has incomplete ownership or permission metadata" ;;
  esac
}

require_path_metadata() {
  metadata_path=$1
  expected_metadata=$2
  read_path_metadata "$metadata_path"
  [ "$path_metadata" = "$expected_metadata" ] ||
    die "$metadata_path must have ownership and permissions $expected_metadata"
}

require_current_config() {
  current_config_path=$1
  config_version_marker=$(
    awk -v expected="$package_version" '
      {
        remaining = $0
        while (match(remaining, /"application_version"[[:space:]]*:/)) {
          seen += 1
          remaining = substr(remaining, RSTART + RLENGTH)
          if (match(remaining, /^[[:space:]]*"[^"]*"/)) {
            value = substr(remaining, RSTART, RLENGTH)
            sub(/^[[:space:]]*"/, "", value)
            sub(/"$/, "", value)
            if (value == expected) valid += 1
          }
        }
      }
      END { printf "%d:%d", seen, valid }
    ' "$current_config_path"
  ) || die "cannot inspect $current_config_path"
  [ "$config_version_marker" = 1:1 ] ||
    die "$current_config_path must contain exactly one current application_version $package_version marker"
}

load_user_marker() {
  marker_format_seen=0
  marker_uid_seen=0
  marker_primary_gid_seen=0
  recorded_user_uid=
  recorded_user_primary_gid=
  while IFS= read -r marker_line || [ -n "$marker_line" ]; do
    case "$marker_line" in
      format="$package_version")
        [ "$marker_format_seen" -eq 0 ] || return 1
        marker_format_seen=1
        ;;
      uid=*)
        [ "$marker_uid_seen" -eq 0 ] || return 1
        recorded_user_uid=${marker_line#uid=}
        marker_uid_seen=1
        ;;
      primary_gid=*)
        [ "$marker_primary_gid_seen" -eq 0 ] || return 1
        recorded_user_primary_gid=${marker_line#primary_gid=}
        marker_primary_gid_seen=1
        ;;
      *) return 1 ;;
    esac
  done <"$managed_user_marker"
  [ "$marker_format_seen" -eq 1 ] && [ "$marker_uid_seen" -eq 1 ] &&
    [ "$marker_primary_gid_seen" -eq 1 ] || return 1
  case "$recorded_user_uid:$recorded_user_primary_gid" in
    *[!0-9:]*) return 1 ;;
    :*|*:) return 1 ;;
  esac
}

load_group_marker() {
  marker_format_seen=0
  marker_gid_seen=0
  recorded_group_gid=
  while IFS= read -r marker_line || [ -n "$marker_line" ]; do
    case "$marker_line" in
      format="$package_version")
        [ "$marker_format_seen" -eq 0 ] || return 1
        marker_format_seen=1
        ;;
      gid=*)
        [ "$marker_gid_seen" -eq 0 ] || return 1
        recorded_group_gid=${marker_line#gid=}
        marker_gid_seen=1
        ;;
      *) return 1 ;;
    esac
  done <"$managed_group_marker"
  [ "$marker_format_seen" -eq 1 ] && [ "$marker_gid_seen" -eq 1 ] || return 1
  case "$recorded_group_gid" in
    ''|*[!0-9]*) return 1 ;;
  esac
}

lookup_user_entry() {
  passwd_listing=$(getent passwd 2>/dev/null) || return 2
  user_entry=
  user_match_count=0
  while IFS= read -r directory_entry || [ -n "$directory_entry" ]; do
    case "$directory_entry" in
      unionc-agent:*)
        user_match_count=$((user_match_count + 1))
        user_entry=$directory_entry
        ;;
    esac
  done <<EOF
$passwd_listing
EOF
  [ "$user_match_count" -eq 1 ] || return 2
}

lookup_group_entry() {
  group_listing=$(getent group 2>/dev/null) || return 2
  group_entry=
  group_match_count=0
  while IFS= read -r directory_entry || [ -n "$directory_entry" ]; do
    case "$directory_entry" in
      unionc-agent:*)
        group_match_count=$((group_match_count + 1))
        group_entry=$directory_entry
        ;;
    esac
  done <<EOF
$group_listing
EOF
  [ "$group_match_count" -eq 1 ] || return 2
}

managed_account_is_still_expected() {
  current_group_gid=$(printf '%s\n' "$group_entry" | cut -d: -f3)
  current_user_uid=$(printf '%s\n' "$user_entry" | cut -d: -f3)
  current_user_gid=$(printf '%s\n' "$user_entry" | cut -d: -f4)
  current_user_home=$(printf '%s\n' "$user_entry" | cut -d: -f6)
  current_user_shell=$(printf '%s\n' "$user_entry" | cut -d: -f7)
  [ "$current_group_gid" = "$recorded_group_gid" ] &&
    [ "$current_user_uid" = "$recorded_user_uid" ] &&
    [ "$current_user_gid" = "$recorded_user_primary_gid" ] &&
    [ "$current_user_gid" = "$current_group_gid" ] &&
    [ "$current_user_home" = "$state_dir" ] &&
    { [ "$current_user_shell" = /usr/sbin/nologin ] ||
      [ "$current_user_shell" = /sbin/nologin ]; }
}

cleanup_backup_temporary() {
  cleanup_status=$?
  trap - EXIT
  if [ -n "$backup_temporary" ]; then
    rm -f -- "$backup_temporary" || true
  fi
  exit "$cleanup_status"
}

save_rpm_config() {
  if [ ! -e "$config_path" ] && [ ! -L "$config_path" ]; then
    return 0
  fi
  [ -d "$config_dir" ] && [ ! -L "$config_dir" ] ||
    die "$config_dir is not a safe package config directory"
  [ -f "$config_path" ] && [ ! -L "$config_path" ] ||
    die "$config_path is not a safe package config file"
  [ -d "$account_state_dir" ] && [ ! -L "$account_state_dir" ] ||
    die "$account_state_dir is not a safe package bookkeeping directory"
  require_path_metadata "$account_state_dir" 0:0:700

  [ -f "$managed_user_marker" ] && [ ! -L "$managed_user_marker" ] ||
    die "managed-user marker is missing or unsafe"
  [ -f "$managed_group_marker" ] && [ ! -L "$managed_group_marker" ] ||
    die "managed-group marker is missing or unsafe"
  require_path_metadata "$managed_user_marker" 0:0:600
  require_path_metadata "$managed_group_marker" 0:0:600
  load_user_marker || die "managed-user marker is invalid"
  load_group_marker || die "managed-group marker is invalid"
  lookup_user_entry && lookup_group_entry && managed_account_is_still_expected ||
    die "package-managed service account identity cannot be verified"

  require_path_metadata "$config_dir" "0:$recorded_group_gid:750"
  require_path_metadata "$config_path" "0:$recorded_group_gid:640"
  require_current_config "$config_path"

  if [ -e "$rpm_config_backup" ] || [ -L "$rpm_config_backup" ]; then
    [ -f "$rpm_config_backup" ] && [ ! -L "$rpm_config_backup" ] ||
      die "RPM config backup is not a safe regular file"
    require_path_metadata "$rpm_config_backup" 0:0:600
    require_current_config "$rpm_config_backup"
  fi

  backup_temporary="$account_state_dir/.config.json.remove-backup.$$"
  trap cleanup_backup_temporary EXIT
  rm -f -- "$backup_temporary"
  umask 077
  cp -p -- "$config_path" "$backup_temporary"
  chown root:root "$backup_temporary"
  chmod 0600 "$backup_temporary"
  require_path_metadata "$backup_temporary" 0:0:600
  require_current_config "$backup_temporary"
  mv -f -- "$backup_temporary" "$rpm_config_backup"
  backup_temporary=
  require_path_metadata "$rpm_config_backup" 0:0:600
  require_current_config "$rpm_config_backup"
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
# the exact same package. Accept only 0.5.0. RPM uses a positive remaining
# instance count for replacement; the new postinstall has already validated
# the exact 0.5.0 ownership markers before the pre-remove scriptlet can run.
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
    save_rpm_config
    disable_for_remove
    ;;
esac
