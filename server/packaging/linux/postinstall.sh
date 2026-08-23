#!/bin/sh
set -eu

PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH

service_name=unionc.service
package_version=0.3.4
server_binary=/usr/bin/unionc
data_dir=/var/lib/unionc
config_dir=/etc/unionc
package_config="$config_dir/unionc.env"
login_defs=/etc/login.defs
account_database_dir=/etc
account_state_dir=/var/lib/unionc-package
managed_user_marker="$account_state_dir/managed-user"
managed_group_marker="$account_state_dir/managed-group"
pending_user_marker="$account_state_dir/pending-user"
pending_group_marker="$account_state_dir/pending-group"
group_marker_state=absent
user_marker_state=absent
group_pending_state=absent
user_pending_state=absent
recorded_group_gid=
recorded_user_uid=
recorded_user_primary_gid=
pending_group_gid=
pending_user_uid=
pending_user_primary_gid=
pending_user_home=
pending_user_shell=

die() {
  echo "unionc postinstall: $*" >&2
  exit 1
}

for command_name in getent groupadd useradd install cut chown chmod rm mv stat sync awk; do
  command -v "$command_name" >/dev/null 2>&1 ||
    die "required command is unavailable: $command_name"
done
[ -x "$server_binary" ] || die "installed Server binary is missing or not executable"

[ "$("$server_binary" --version)" = "unionc $package_version" ] ||
  die "installed binary version does not match package lifecycle version $package_version"

read_path_metadata() {
  metadata_path=$1
  path_metadata=$(stat -c '%u:%g:%a:%h' -- "$metadata_path") ||
    die "cannot read ownership and permissions for $metadata_path"
  path_uid=${path_metadata%%:*}
  path_metadata_remainder=${path_metadata#*:}
  path_gid=${path_metadata_remainder%%:*}
  path_metadata_remainder=${path_metadata_remainder#*:}
  path_mode=${path_metadata_remainder%%:*}
  path_nlink=${path_metadata_remainder#*:}
  case "$path_uid:$path_gid:$path_mode:$path_nlink" in
    *[!0-9:]*) die "$metadata_path has invalid ownership or permission metadata" ;;
    :*|*::*|*:) die "$metadata_path has incomplete ownership or permission metadata" ;;
  esac
  case "$path_mode" in
    ''|*[!0-7]*) die "$metadata_path has an invalid permission mode" ;;
  esac
}

require_current_package_config() {
  package_config_marker=$(
    awk -v expected="UNIONC_PACKAGE_VERSION=$package_version" '
      /^[[:space:]]*(export[[:space:]]+)?UNIONC_PACKAGE_VERSION[[:space:]]*=/ {
        seen += 1
        if ($0 == expected) valid += 1
      }
      END { printf "%d:%d", seen, valid }
    ' "$package_config"
  ) || die "cannot inspect $package_config"
  [ "$package_config_marker" = 1:1 ] ||
    die "$package_config must contain exactly one current UNIONC_PACKAGE_VERSION=$package_version marker"
}

# nFPM lays down this config before invoking postinstall. Validate the protected
# parent and file metadata before reading it or creating any account. A retained
# config may still use the numeric group recorded by this exact package version;
# that relationship is checked after the protected markers have been loaded.
[ -d "$config_dir" ] && [ ! -L "$config_dir" ] ||
  die "$config_dir is not a safe current-package config directory"
read_path_metadata "$config_dir"
[ "$path_uid:$path_gid" = 0:0 ] ||
  die "$config_dir must be owned by root:root"
[ "$((0$path_mode & 022))" -eq 0 ] ||
  die "$config_dir must not be writable by group or other users"
initial_config_dir_metadata="$path_uid:$path_gid:$path_mode"

[ -f "$package_config" ] && [ ! -L "$package_config" ] ||
  die "$package_config is not a safe current-package config file"
read_path_metadata "$package_config"
[ "$path_uid:$path_mode:$path_nlink" = 0:640:1 ] ||
  die "$package_config must be root-owned with permissions 0640 and one hard link"
initial_package_config_gid=$path_gid
initial_package_config_metadata="$path_uid:$path_gid:$path_mode:$path_nlink"
require_current_package_config

[ -d "$account_database_dir" ] && [ ! -L "$account_database_dir" ] ||
  die "$account_database_dir is not a safe account database directory"
read_path_metadata "$account_database_dir"
[ "$path_uid:$path_gid" = 0:0 ] ||
  die "$account_database_dir must be owned by root:root"
[ "$((0$path_mode & 022))" -eq 0 ] ||
  die "$account_database_dir must not be writable by group or other users"

load_system_id_ranges() {
  [ -f "$login_defs" ] && [ ! -L "$login_defs" ] ||
    die "$login_defs is not a safe regular file"
  read_path_metadata "$login_defs"
  [ "$path_uid" = 0 ] && [ "$path_nlink" -eq 1 ] ||
    die "$login_defs must be root-owned with one hard link"
  [ "$((0$path_mode & 022))" -eq 0 ] ||
    die "$login_defs must not be writable by group or other users"

  system_id_ranges=$(
    awk '
      function remember(key, value) {
        if (seen[key] || value !~ /^[0-9]+$/) {
          invalid = 1
          exit
        }
        seen[key] = 1
        values[key] = value + 0
      }
      /^[[:space:]]*#/ || NF == 0 { next }
      $1 == "UID_MIN" || $1 == "SYS_UID_MIN" || $1 == "SYS_UID_MAX" ||
      $1 == "GID_MIN" || $1 == "SYS_GID_MIN" || $1 == "SYS_GID_MAX" {
        remember($1, $2)
      }
      END {
        if (invalid) exit 2
        uid_regular_min = seen["UID_MIN"] ? values["UID_MIN"] : 1000
        gid_regular_min = seen["GID_MIN"] ? values["GID_MIN"] : 1000
        uid_min = seen["SYS_UID_MIN"] ? values["SYS_UID_MIN"] : 101
        uid_max = seen["SYS_UID_MAX"] ? values["SYS_UID_MAX"] : uid_regular_min - 1
        gid_min = seen["SYS_GID_MIN"] ? values["SYS_GID_MIN"] : 101
        gid_max = seen["SYS_GID_MAX"] ? values["SYS_GID_MAX"] : gid_regular_min - 1
        if (uid_min < 1 || uid_max < uid_min || uid_max > 4294967294 ||
            gid_min < 1 || gid_max < gid_min || gid_max > 4294967294 ||
            uid_max - uid_min > 1000000 || gid_max - gid_min > 1000000) {
          exit 2
        }
        printf "%.0f:%.0f:%.0f:%.0f", uid_min, uid_max, gid_min, gid_max
      }
    ' "$login_defs"
  ) || die "$login_defs has invalid or ambiguous system identity ranges"
  system_uid_min=${system_id_ranges%%:*}
  system_id_remainder=${system_id_ranges#*:}
  system_uid_max=${system_id_remainder%%:*}
  system_id_remainder=${system_id_remainder#*:}
  system_gid_min=${system_id_remainder%%:*}
  system_gid_max=${system_id_remainder#*:}
}

select_free_group_gid() {
  selection_passwd_listing=$(getent passwd 2>/dev/null) ||
    die "cannot enumerate users while selecting a dedicated group gid"
  pending_group_gid=$(
    printf '%s\n__UNIONC_PASSWD_SECTION__\n%s\n' "$group_listing" "$selection_passwd_listing" |
      awk -F: -v minimum="$system_gid_min" -v maximum="$system_gid_max" '
        $0 == "__UNIONC_PASSWD_SECTION__" { passwd_section = 1; next }
        NF == 0 { next }
        !passwd_section {
          if (NF != 4 || $3 !~ /^[0-9]+$/) { invalid = 1; next }
          used[$3 + 0] = 1
          next
        }
        {
          if (NF != 7 || $4 !~ /^[0-9]+$/) { invalid = 1; next }
          used[$4 + 0] = 1
        }
        END {
          if (invalid) exit 2
          for (candidate = maximum; candidate >= minimum; candidate -= 1) {
            if (!(candidate in used)) {
              printf "%.0f", candidate
              exit 0
            }
          }
          exit 3
        }
      '
  ) || die "no unambiguous system gid is available for the dedicated group"
}

select_free_user_uid() {
  pending_user_uid=$(
    printf '%s\n' "$passwd_listing" |
      awk -F: -v minimum="$system_uid_min" -v maximum="$system_uid_max" '
        NF == 0 { next }
        NF != 7 || $3 !~ /^[0-9]+$/ { invalid = 1; next }
        { used[$3 + 0] = 1 }
        END {
          if (invalid) exit 2
          for (candidate = maximum; candidate >= minimum; candidate -= 1) {
            if (!(candidate in used)) {
              printf "%.0f", candidate
              exit 0
            }
          }
          exit 3
        }
      '
  ) || die "no unambiguous system uid is available for the dedicated user"
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

load_pending_group() {
  pending_format_seen=0
  pending_state_seen=0
  pending_kind_seen=0
  pending_name_seen=0
  pending_gid_seen=0
  pending_group_gid=
  while IFS= read -r pending_line || [ -n "$pending_line" ]; do
    case "$pending_line" in
      format="$package_version")
        [ "$pending_format_seen" -eq 0 ] || return 1
        pending_format_seen=1
        ;;
      state=pending)
        [ "$pending_state_seen" -eq 0 ] || return 1
        pending_state_seen=1
        ;;
      kind=group)
        [ "$pending_kind_seen" -eq 0 ] || return 1
        pending_kind_seen=1
        ;;
      name=unionc)
        [ "$pending_name_seen" -eq 0 ] || return 1
        pending_name_seen=1
        ;;
      gid=*)
        [ "$pending_gid_seen" -eq 0 ] || return 1
        pending_group_gid=${pending_line#gid=}
        pending_gid_seen=1
        ;;
      *) return 1 ;;
    esac
  done <"$pending_group_marker"
  [ "$pending_format_seen:$pending_state_seen:$pending_kind_seen:$pending_name_seen:$pending_gid_seen" = 1:1:1:1:1 ] ||
    return 1
  case "$pending_group_gid" in
    ''|*[!0-9]*) return 1 ;;
  esac
}

load_pending_user() {
  pending_format_seen=0
  pending_state_seen=0
  pending_kind_seen=0
  pending_name_seen=0
  pending_uid_seen=0
  pending_primary_gid_seen=0
  pending_home_seen=0
  pending_shell_seen=0
  pending_user_primary_gid=
  pending_user_uid=
  pending_user_home=
  pending_user_shell=
  while IFS= read -r pending_line || [ -n "$pending_line" ]; do
    case "$pending_line" in
      format="$package_version")
        [ "$pending_format_seen" -eq 0 ] || return 1
        pending_format_seen=1
        ;;
      state=pending)
        [ "$pending_state_seen" -eq 0 ] || return 1
        pending_state_seen=1
        ;;
      kind=user)
        [ "$pending_kind_seen" -eq 0 ] || return 1
        pending_kind_seen=1
        ;;
      name=unionc)
        [ "$pending_name_seen" -eq 0 ] || return 1
        pending_name_seen=1
        ;;
      uid=*)
        [ "$pending_uid_seen" -eq 0 ] || return 1
        pending_user_uid=${pending_line#uid=}
        pending_uid_seen=1
        ;;
      primary_gid=*)
        [ "$pending_primary_gid_seen" -eq 0 ] || return 1
        pending_user_primary_gid=${pending_line#primary_gid=}
        pending_primary_gid_seen=1
        ;;
      home=*)
        [ "$pending_home_seen" -eq 0 ] || return 1
        pending_user_home=${pending_line#home=}
        pending_home_seen=1
        ;;
      shell=*)
        [ "$pending_shell_seen" -eq 0 ] || return 1
        pending_user_shell=${pending_line#shell=}
        pending_shell_seen=1
        ;;
      *) return 1 ;;
    esac
  done <"$pending_user_marker"
  [ "$pending_format_seen:$pending_state_seen:$pending_kind_seen:$pending_name_seen:$pending_uid_seen" = 1:1:1:1:1 ] &&
    [ "$pending_primary_gid_seen:$pending_home_seen:$pending_shell_seen" = 1:1:1 ] || return 1
  case "$pending_user_uid:$pending_user_primary_gid" in
    *[!0-9:]*) return 1 ;;
    :*|*:) return 1 ;;
  esac
  [ "$pending_user_home" = "$data_dir" ] || return 1
  case "$pending_user_shell" in
    /usr/sbin/nologin|/sbin/nologin) ;;
    *) return 1 ;;
  esac
}

require_safe_state_file() {
  state_file=$1
  state_description=$2
  [ -f "$state_file" ] && [ ! -L "$state_file" ] ||
    die "$state_description is not a safe regular file"
  read_path_metadata "$state_file"
  [ "$path_uid:$path_gid:$path_mode:$path_nlink" = 0:0:600:1 ] ||
    die "$state_description must be owned by root:root with permissions 0600 and one hard link"
  state_file_size=$(stat -c %s -- "$state_file") ||
    die "cannot read the size of $state_description"
  case "$state_file_size" in
    ''|*[!0-9]*) die "$state_description has an invalid size" ;;
  esac
  [ "$state_file_size" -le 512 ] || die "$state_description is unexpectedly large"
}

inspect_existing_markers() {
  if [ -e "$managed_group_marker" ] || [ -L "$managed_group_marker" ]; then
    require_safe_state_file "$managed_group_marker" "managed group marker"
    load_group_marker || die "managed group marker is not for UnionC $package_version"
    group_marker_state=valid
  fi

  if [ -e "$managed_user_marker" ] || [ -L "$managed_user_marker" ]; then
    require_safe_state_file "$managed_user_marker" "managed user marker"
    load_user_marker || die "managed user marker is not for UnionC $package_version"
    user_marker_state=valid
  fi

  if [ -e "$pending_group_marker" ] || [ -L "$pending_group_marker" ]; then
    require_safe_state_file "$pending_group_marker" "pending group marker"
    load_pending_group || die "pending group marker is not for UnionC $package_version"
    group_pending_state=valid
  fi

  if [ -e "$pending_user_marker" ] || [ -L "$pending_user_marker" ]; then
    require_safe_state_file "$pending_user_marker" "pending user marker"
    load_pending_user || die "pending user marker is not for UnionC $package_version"
    user_pending_state=valid
  fi

  if [ "$user_marker_state" = valid ] && [ "$group_marker_state" != valid ]; then
    die "managed user marker exists without its managed group marker"
  fi
  if [ "$user_pending_state" = valid ] && [ "$group_marker_state" != valid ]; then
    die "pending user marker exists without a committed managed group marker"
  fi
}

prepare_state_temporary() {
  state_temporary=$1
  state_description=$2
  chown root:root "$state_temporary"
  chmod 0600 "$state_temporary"
  require_safe_state_file "$state_temporary" "$state_description temporary"
  sync -f -- "$state_temporary" || die "cannot persist $state_description temporary"
}

publish_state_temporary() {
  state_temporary=$1
  state_destination=$2
  state_description=$3
  state_move_status=0
  if mv -f -- "$state_temporary" "$state_destination"; then
    state_move_status=0
  else
    state_move_status=$?
  fi

  # A wrapper may report failure after rename completed. The durable on-disk
  # state, not mv's exit status or an in-memory flag, is the commit authority.
  if [ ! -f "$state_destination" ] || [ -L "$state_destination" ]; then
    rm -f -- "$state_temporary" || :
    die "cannot publish $state_description (mv status $state_move_status)"
  fi
  require_safe_state_file "$state_destination" "$state_description"
  sync -f -- "$account_state_dir" || die "cannot persist $state_description directory entry"
  if [ -e "$state_temporary" ] || [ -L "$state_temporary" ]; then
    rm -f -- "$state_temporary" || die "cannot remove stale $state_description temporary"
    sync -f -- "$account_state_dir" || die "cannot persist temporary cleanup for $state_description"
  fi
}

write_group_marker() {
  marker_gid=$1
  marker_temporary="$account_state_dir/.managed-group.new"
  umask 077
  (
    set -C
    printf 'format=%s\ngid=%s\n' "$package_version" "$marker_gid" >"$marker_temporary"
  ) || die "cannot create managed group marker temporary"
  prepare_state_temporary "$marker_temporary" "managed group marker"
  publish_state_temporary "$marker_temporary" "$managed_group_marker" "managed group marker"
  load_group_marker && [ "$recorded_group_gid" = "$marker_gid" ] ||
    die "published managed group marker does not match the dedicated group"
  group_marker_state=valid
}

write_user_marker() {
  marker_uid=$1
  marker_primary_gid=$2
  marker_temporary="$account_state_dir/.managed-user.new"
  umask 077
  (
    set -C
    printf 'format=%s\nuid=%s\nprimary_gid=%s\n' \
      "$package_version" "$marker_uid" "$marker_primary_gid" >"$marker_temporary"
  ) || die "cannot create managed user marker temporary"
  prepare_state_temporary "$marker_temporary" "managed user marker"
  publish_state_temporary "$marker_temporary" "$managed_user_marker" "managed user marker"
  load_user_marker && [ "$recorded_user_uid:$recorded_user_primary_gid" = "$marker_uid:$marker_primary_gid" ] ||
    die "published managed user marker does not match the dedicated user"
  user_marker_state=valid
}

write_pending_group() {
  requested_group_gid=$1
  pending_temporary="$account_state_dir/.pending-group.new"
  umask 077
  (
    set -C
    printf 'format=%s\nstate=pending\nkind=group\nname=unionc\ngid=%s\n' \
      "$package_version" "$requested_group_gid" >"$pending_temporary"
  ) || die "cannot create pending group marker temporary"
  prepare_state_temporary "$pending_temporary" "pending group marker"
  publish_state_temporary "$pending_temporary" "$pending_group_marker" "pending group marker"
  load_pending_group && [ "$pending_group_gid" = "$requested_group_gid" ] ||
    die "published pending group marker does not match the requested gid"
  group_pending_state=valid
}

write_pending_user() {
  requested_user_uid=$1
  requested_primary_gid=$2
  requested_shell=$3
  pending_temporary="$account_state_dir/.pending-user.new"
  umask 077
  (
    set -C
    printf 'format=%s\nstate=pending\nkind=user\nname=unionc\nuid=%s\nprimary_gid=%s\nhome=%s\nshell=%s\n' \
      "$package_version" "$requested_user_uid" "$requested_primary_gid" "$data_dir" \
      "$requested_shell" >"$pending_temporary"
  ) || die "cannot create pending user marker temporary"
  prepare_state_temporary "$pending_temporary" "pending user marker"
  publish_state_temporary "$pending_temporary" "$pending_user_marker" "pending user marker"
  load_pending_user &&
    [ "$pending_user_uid:$pending_user_primary_gid:$pending_user_shell" = \
      "$requested_user_uid:$requested_primary_gid:$requested_shell" ] ||
    die "published pending user marker does not match the requested account"
  user_pending_state=valid
}

clear_pending_group() {
  require_safe_state_file "$pending_group_marker" "pending group marker"
  load_pending_group || die "pending group marker changed before cleanup"
  rm -f -- "$pending_group_marker" || die "cannot remove committed pending group marker"
  sync -f -- "$account_state_dir" || die "cannot persist pending group marker cleanup"
  group_pending_state=absent
}

clear_pending_user() {
  require_safe_state_file "$pending_user_marker" "pending user marker"
  load_pending_user || die "pending user marker changed before cleanup"
  rm -f -- "$pending_user_marker" || die "cannot remove committed pending user marker"
  sync -f -- "$account_state_dir" || die "cannot persist pending user marker cleanup"
  user_pending_state=absent
}

discard_incomplete_state_temporaries() {
  for incomplete_state_temporary in \
    "$account_state_dir/.pending-group.new" \
    "$account_state_dir/.pending-user.new" \
    "$account_state_dir/.managed-group.new" \
    "$account_state_dir/.managed-user.new"; do
    if [ -e "$incomplete_state_temporary" ] || [ -L "$incomplete_state_temporary" ]; then
      require_safe_state_file "$incomplete_state_temporary" "incomplete package state temporary"
      rm -f -- "$incomplete_state_temporary" ||
        die "cannot remove incomplete package state temporary"
      sync -f -- "$account_state_dir" ||
        die "cannot persist incomplete package state cleanup"
    fi
  done
}

# Enumerate NSS for both normal creation and forward recovery. A failed keyed
# lookup is not proof that an account is absent, and a partially completed
# package transaction must never adopt an ambiguous identity.
lookup_user_entry() {
  passwd_listing=$(getent passwd 2>/dev/null) || return 2
  user_entry=
  user_match_count=0
  while IFS= read -r directory_entry || [ -n "$directory_entry" ]; do
    case "$directory_entry" in
      unionc:*)
        user_match_count=$((user_match_count + 1))
        user_entry=$directory_entry
        ;;
    esac
  done <<EOF
$passwd_listing
EOF
  case "$user_match_count" in
    0) return 1 ;;
    1) return 0 ;;
    *) return 2 ;;
  esac
}

lookup_group_entry() {
  group_listing=$(getent group 2>/dev/null) || return 2
  group_entry=
  group_match_count=0
  while IFS= read -r directory_entry || [ -n "$directory_entry" ]; do
    case "$directory_entry" in
      unionc:*)
        group_match_count=$((group_match_count + 1))
        group_entry=$directory_entry
        ;;
    esac
  done <<EOF
$group_listing
EOF
  case "$group_match_count" in
    0) return 1 ;;
    1) return 0 ;;
    *) return 2 ;;
  esac
}

read_group_identity() {
  group_field_count=$(printf '%s\n' "$group_entry" | awk -F: 'NR == 1 { print NF }') || return 2
  [ "$group_field_count" -eq 4 ] || return 1
  current_group_name=$(printf '%s\n' "$group_entry" | cut -d: -f1)
  current_group_gid=$(printf '%s\n' "$group_entry" | cut -d: -f3)
  current_group_members=$(printf '%s\n' "$group_entry" | cut -d: -f4)
  [ "$current_group_name" = unionc ] || return 1
  case "$current_group_gid" in
    ''|*[!0-9]*) return 1 ;;
  esac
}

group_gid_is_unique() {
  group_gid_match_count=$(
    printf '%s\n' "$group_listing" |
      awk -F: -v expected_gid="$current_group_gid" '
        NF == 0 { next }
        NF != 4 { invalid = 1 }
        $3 == expected_gid { matches += 1 }
        END {
          if (invalid) exit 2
          print matches + 0
        }
      '
  ) || return 2
  [ "$group_gid_match_count" -eq 1 ]
}

# Before the user phase starts, a group recovered from a durable pending intent
# must still be the unique, empty identity that groupadd was asked to create.
pending_group_account_is_exact() {
  read_group_identity || return $?
  [ "$current_group_gid" = "$pending_group_gid" ] || return 1
  group_gid_is_unique || return $?
  [ -z "$current_group_members" ] || return 1
  current_passwd_listing=$(getent passwd 2>/dev/null) || return 2
  primary_gid_match_count=$(
    printf '%s\n' "$current_passwd_listing" |
      awk -F: -v expected_gid="$current_group_gid" '
        NF == 0 { next }
        NF != 7 { invalid = 1 }
        $4 == expected_gid { matches += 1 }
        END {
          if (invalid) exit 2
          print matches + 0
        }
      '
  ) || return 2
  [ "$primary_gid_match_count" -eq 0 ] || return 1

  gshadow_listing=$(getent gshadow 2>/dev/null) || return 2
  gshadow_entry=
  gshadow_match_count=0
  while IFS= read -r directory_entry || [ -n "$directory_entry" ]; do
    case "$directory_entry" in
      unionc:*)
        gshadow_match_count=$((gshadow_match_count + 1))
        gshadow_entry=$directory_entry
        ;;
    esac
  done <<EOF
$gshadow_listing
EOF
  [ "$gshadow_match_count" -eq 1 ] || return 1
  gshadow_field_count=$(printf '%s\n' "$gshadow_entry" | awk -F: 'NR == 1 { print NF }') ||
    return 2
  [ "$gshadow_field_count" -eq 4 ] || return 1
  gshadow_password=$(printf '%s\n' "$gshadow_entry" | cut -d: -f2)
  gshadow_administrators=$(printf '%s\n' "$gshadow_entry" | cut -d: -f3)
  gshadow_members=$(printf '%s\n' "$gshadow_entry" | cut -d: -f4)
  case "$gshadow_password" in
    '!'*|'*'*) ;;
    *) return 1 ;;
  esac
  [ -z "$gshadow_administrators" ] && [ -z "$gshadow_members" ]
}

read_user_identity() {
  user_field_count=$(printf '%s\n' "$user_entry" | awk -F: 'NR == 1 { print NF }') || return 2
  [ "$user_field_count" -eq 7 ] || return 1
  current_uid=$(printf '%s\n' "$user_entry" | cut -d: -f3)
  current_gid=$(printf '%s\n' "$user_entry" | cut -d: -f4)
  current_home=$(printf '%s\n' "$user_entry" | cut -d: -f6)
  current_shell=$(printf '%s\n' "$user_entry" | cut -d: -f7)
  case "$current_uid:$current_gid" in
    *[!0-9:]*) return 1 ;;
    :*|*:) return 1 ;;
  esac
}

user_uid_is_unique() {
  user_uid_match_count=$(
    printf '%s\n' "$passwd_listing" |
      awk -F: -v expected_uid="$current_uid" '
        NF == 0 { next }
        NF != 7 { invalid = 1 }
        $3 == expected_uid { matches += 1 }
        END {
          if (invalid) exit 2
          print matches + 0
        }
      '
  ) || return 2
  [ "$user_uid_match_count" -eq 1 ]
}

pending_user_account_is_exact() {
  read_user_identity || return $?
  [ "$current_uid" = "$pending_user_uid" ] || return 1
  user_uid_is_unique || return $?
  [ "$current_gid" = "$pending_user_primary_gid" ] &&
    [ "$current_home" = "$pending_user_home" ] &&
    [ "$current_shell" = "$pending_user_shell" ] || return 1

  shadow_listing=$(getent shadow 2>/dev/null) || return 2
  shadow_entry=
  shadow_match_count=0
  while IFS= read -r directory_entry || [ -n "$directory_entry" ]; do
    case "$directory_entry" in
      unionc:*)
        shadow_match_count=$((shadow_match_count + 1))
        shadow_entry=$directory_entry
        ;;
    esac
  done <<EOF
$shadow_listing
EOF
  [ "$shadow_match_count" -eq 1 ] || return 1
  shadow_field_count=$(printf '%s\n' "$shadow_entry" | awk -F: 'NR == 1 { print NF }') ||
    return 2
  [ "$shadow_field_count" -eq 9 ] || return 1
  shadow_password=$(printf '%s\n' "$shadow_entry" | cut -d: -f2)
  case "$shadow_password" in
    '!'*|'*'*) return 0 ;;
    *) return 1 ;;
  esac
}

# The root-owned marker directory binds the current package version to the
# exact numeric service identity. Removing the package deliberately keeps it,
# so only an exact 0.3.4 reinstall can reclaim the retained state. Never
# normalize an existing foreign directory before trusting marker files below.
if [ -e "$account_state_dir" ] || [ -L "$account_state_dir" ]; then
  [ -d "$account_state_dir" ] && [ ! -L "$account_state_dir" ] ||
    die "package account state path is not a safe directory"
  read_path_metadata "$account_state_dir"
  [ "$path_uid:$path_gid:$path_mode" = 0:0:700 ] ||
    die "package account state directory must be owned by root:root with permissions 0700"
else
  install -d -m 0700 -o root -g root "$account_state_dir"
fi
[ -d "$account_state_dir" ] && [ ! -L "$account_state_dir" ] ||
  die "package account state path did not become a safe directory"
read_path_metadata "$account_state_dir"
[ "$path_uid:$path_gid:$path_mode" = 0:0:700 ] ||
  die "package account state directory was not created as root:root with permissions 0700"
discard_incomplete_state_temporaries
inspect_existing_markers

if [ "$initial_package_config_gid" != 0 ]; then
  [ "$group_marker_state" = valid ] &&
    [ "$initial_package_config_gid" = "$recorded_group_gid" ] ||
    die "$package_config has no current package-managed group ownership"
fi

data_dir_preexisting=0
if [ -e "$data_dir" ] || [ -L "$data_dir" ]; then
  [ -d "$data_dir" ] && [ ! -L "$data_dir" ] ||
    die "$data_dir is not a safe directory"
  data_dir_preexisting=1
  [ "$group_marker_state" = valid ] && [ "$user_marker_state" = valid ] ||
    die "refusing to adopt pre-existing $data_dir without current package ownership markers"
fi

group_lookup_status=0
if lookup_group_entry; then
  group_lookup_status=0
else
  group_lookup_status=$?
  [ "$group_lookup_status" -eq 1 ] ||
    die "dedicated group database is unavailable or ambiguous"
  [ "$group_marker_state" = absent ] ||
    die "package-managed unionc group is missing"
  if [ "$group_pending_state" = absent ]; then
    load_system_id_ranges
    select_free_group_gid
    write_pending_group "$pending_group_gid"
  fi

  groupadd_status=0
  if groupadd --system --gid "$pending_group_gid" unionc; then
    groupadd_status=0
  else
    groupadd_status=$?
  fi
  group_lookup_status=0
  if lookup_group_entry; then
    group_lookup_status=0
  else
    group_lookup_status=$?
    if [ "$group_lookup_status" -eq 1 ]; then
      die "dedicated group creation did not produce an account (groupadd status $groupadd_status);" \
        "pending recovery was preserved"
    fi
    die "new dedicated group could not be enumerated; pending recovery was preserved"
  fi
fi
read_group_identity || die "dedicated group has an invalid account structure"
group_gid_is_unique || die "dedicated group gid is ambiguous"
group_gid=$current_group_gid
if [ "$group_marker_state" = valid ]; then
  [ "$recorded_group_gid" = "$group_gid" ] ||
    die "package-managed unionc group was replaced with a different gid"
elif [ "$group_pending_state" = valid ]; then
  pending_group_account_is_exact ||
    die "pending dedicated group does not match a unique unused package identity"
  # shadow-utils fsyncs the replacement file but does not guarantee that the
  # /etc directory rename is durable. This barrier must precede a marker that
  # may live on another filesystem.
  sync -f -- "$account_database_dir" ||
    die "cannot persist the dedicated group account before committing its marker"
  write_group_marker "$group_gid"
else
  die "existing unionc group has no current $package_version ownership marker or pending intent"
fi

# Re-read the committed identity before removing the write-ahead intent. If a
# crash occurs first, the next invocation treats the marker as authoritative
# and only finishes this cleanup.
lookup_group_entry || die "committed dedicated group could not be enumerated"
read_group_identity || die "committed dedicated group has an invalid account structure"
group_gid_is_unique || die "committed dedicated group gid is ambiguous"
group_gid=$current_group_gid
[ "$recorded_group_gid" = "$group_gid" ] ||
  die "package-managed unionc group was replaced with a different gid"
if [ "$group_pending_state" = valid ]; then
  [ "$pending_group_gid" = "$group_gid" ] ||
    die "pending dedicated group conflicts with the committed identity"
  clear_pending_group
fi

user_lookup_status=0
if lookup_user_entry; then
  user_lookup_status=0
else
  user_lookup_status=$?
  [ "$user_lookup_status" -eq 1 ] ||
    die "dedicated user database is unavailable or ambiguous"
  [ "$user_marker_state" = absent ] ||
    die "package-managed unionc user is missing"
  if [ "$user_pending_state" = absent ]; then
    load_system_id_ranges
    select_free_user_uid
    nologin_shell=/usr/sbin/nologin
    if [ ! -x "$nologin_shell" ]; then
      nologin_shell=/sbin/nologin
    fi
    [ -x "$nologin_shell" ] || die "neither /usr/sbin/nologin nor /sbin/nologin exists"
    write_pending_user "$pending_user_uid" "$group_gid" "$nologin_shell"
  fi

  [ "$pending_user_primary_gid" = "$group_gid" ] ||
    die "pending dedicated user targets a different primary group"
  [ -x "$pending_user_shell" ] ||
    die "pending dedicated user nologin shell is unavailable"
  useradd_status=0
  if useradd --system --uid "$pending_user_uid" --gid unionc --home-dir "$pending_user_home" \
    --shell "$pending_user_shell" unionc; then
    useradd_status=0
  else
    useradd_status=$?
  fi
  user_lookup_status=0
  if lookup_user_entry; then
    user_lookup_status=0
  else
    user_lookup_status=$?
    if [ "$user_lookup_status" -eq 1 ]; then
      die "dedicated user creation did not produce an account (useradd status $useradd_status);" \
        "pending recovery was preserved"
    fi
    die "new dedicated user could not be enumerated; pending recovery was preserved"
  fi
fi

read_user_identity || die "dedicated user has an invalid account structure"
user_uid_is_unique || die "dedicated user uid is ambiguous"
user_uid=$current_uid
user_gid=$current_gid
user_home=$current_home
user_shell=$current_shell
[ "$user_gid" = "$group_gid" ] || die "unionc user does not use the dedicated group"
[ "$user_home" = "$data_dir" ] || die "unionc user has an unexpected home"
case "$user_shell" in
  /usr/sbin/nologin|/sbin/nologin) ;;
  *) die "unionc user has an interactive or unexpected shell" ;;
esac

if [ "$user_marker_state" = valid ]; then
  if
    [ "$recorded_user_uid" != "$user_uid" ] ||
      [ "$recorded_user_primary_gid" != "$user_gid" ]; then
    die "package-managed unionc user was replaced with a different numeric identity"
  fi
elif [ "$user_pending_state" = valid ]; then
  pending_user_account_is_exact ||
    die "pending dedicated user does not match the package creation intent"
  sync -f -- "$account_database_dir" ||
    die "cannot persist the dedicated user account before committing its marker"
  write_user_marker "$user_uid" "$user_gid"
else
  die "existing unionc user has no current $package_version ownership marker or pending intent"
fi

lookup_user_entry || die "committed dedicated user could not be enumerated"
read_user_identity || die "committed dedicated user has an invalid account structure"
user_uid_is_unique || die "committed dedicated user uid is ambiguous"
user_uid=$current_uid
user_gid=$current_gid
user_home=$current_home
user_shell=$current_shell
[ "$recorded_user_uid:$recorded_user_primary_gid" = "$user_uid:$user_gid" ] ||
  die "committed dedicated user no longer matches its marker"
[ "$user_gid" = "$group_gid" ] || die "committed unionc user uses a different primary group"
[ "$user_home" = "$data_dir" ] || die "committed unionc user has an unexpected home"
case "$user_shell" in
  /usr/sbin/nologin|/sbin/nologin) ;;
  *) die "committed unionc user has an unexpected shell" ;;
esac
if [ "$user_pending_state" = valid ]; then
  [ "$pending_user_uid:$pending_user_primary_gid:$pending_user_home:$pending_user_shell" = \
    "$user_uid:$user_gid:$user_home:$user_shell" ] ||
    die "pending dedicated user conflicts with the committed identity"
  clear_pending_user
fi

if [ "$data_dir_preexisting" -eq 1 ]; then
  data_uid=$(stat -c %u "$data_dir")
  data_gid=$(stat -c %g "$data_dir")
  data_mode=$(stat -c %a "$data_dir")
  [ "$data_uid" = "$user_uid" ] && [ "$data_gid" = "$user_gid" ] ||
    die "$data_dir is not owned by the recorded UnionC identity"
  [ "$data_mode" = 700 ] || die "$data_dir permissions must be 0700"
else
  install -d -m 0700 -o unionc -g unionc "$data_dir"
fi

# The trusted parent prevents an unprivileged replacement, but repeat every
# path, marker, and metadata check before the root chown/chmod commit point.
[ -d "$config_dir" ] && [ ! -L "$config_dir" ] ||
  die "$config_dir was redirected during postinstall"
read_path_metadata "$config_dir"
[ "$path_uid:$path_gid:$path_mode" = "$initial_config_dir_metadata" ] ||
  die "$config_dir metadata changed during postinstall"
[ -f "$package_config" ] && [ ! -L "$package_config" ] ||
  die "$package_config was redirected during postinstall"
require_current_package_config
read_path_metadata "$package_config"
[ "$path_uid:$path_gid:$path_mode:$path_nlink" = "$initial_package_config_metadata" ] ||
  die "$package_config metadata changed during postinstall"
chown "root:$user_gid" "$package_config"
chmod 0640 "$package_config"
read_path_metadata "$package_config"
[ "$path_uid:$path_gid:$path_mode:$path_nlink" = "0:$user_gid:640:1" ] ||
  die "$package_config could not be secured for the recorded UnionC group"

if [ -d /run/systemd/system ]; then
  command -v systemctl >/dev/null 2>&1 || die "systemd is running but systemctl is unavailable"
  systemctl daemon-reload
fi

# Fresh installs remain disabled until the administrator configures the
# production secret. Reinstalling this exact package restarts an already
# enabled service without enabling a previously disabled installation.
service_was_enabled=0
if [ -d /run/systemd/system ]; then
  if service_enablement=$(LC_ALL=C systemctl is-enabled "$service_name" 2>/dev/null); then
    service_enablement_status=0
  else
    service_enablement_status=$?
  fi
  case "$service_enablement" in
    enabled|enabled-runtime)
      [ "$service_enablement_status" -eq 0 ] ||
        die "systemctl returned a contradictory enabled state for $service_name"
      service_was_enabled=1
      ;;
    disabled|masked|masked-runtime)
      [ "$service_enablement_status" -ne 0 ] ||
        die "systemctl returned a contradictory disabled state for $service_name"
      ;;
    *)
      die "failed to determine whether $service_name was enabled (systemctl status $service_enablement_status)"
      ;;
  esac
fi
if [ "$service_was_enabled" -eq 1 ]; then
  systemctl restart "$service_name" ||
    die "enabled service did not reach readiness after reinstall; inspect systemctl status unionc and journalctl -u unionc"
  systemctl is-active --quiet "$service_name" ||
    die "enabled service did not remain active after reinstall"
elif [ ! -e "$data_dir/unionc.db" ]; then
  cat <<'EOF'

UnionC 已安装。首次启动前请完成：

  1. 编辑 /etc/unionc/unionc.env，分别填入两个不可复用的随机值
       openssl rand -base64 32        # UNIONC_SECRET_KEY
       openssl rand -hex 32           # UNIONC_PROXY_SECRET
     并把同一个 UNIONC_PROXY_SECRET 安全地配置到可信反向代理环境。
  2. 首次部署临时打开 UNIONC_ALLOW_BOOTSTRAP=1 与 UNIONC_BOOTSTRAP_PASSWORD
  3. systemctl enable --now unionc
  4. 管理员配置与数据库创建完成后，从 unionc.env 删除上述两个 bootstrap 变量并 restart

显式首次 bootstrap 会创建 /var/lib/unionc/unionc.db；无需安装或配置数据库服务。
普通生产启动不会重建缺失或空数据库，而会失败并要求核对数据目录或执行 restore。
数据目录固定为 /var/lib/unionc（由 unit 中的 UNIONC_DATA_DIR 指定）。
UnionC 强制绑定回环，请在其前部署 HTTPS 反向代理；完整请求头契约见
docs/examples/caddy/Caddyfile.console.example。

EOF
else
  cat <<'EOF'

UnionC 已重新安装；服务继续保持 disabled。需要启动时请执行：

  systemctl enable --now unionc

EOF
fi
