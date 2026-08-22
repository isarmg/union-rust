#!/bin/sh
set -eu

PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH

service_name=unionc.service
package_version=0.3.2
server_binary=/usr/bin/unionc
data_dir=/var/lib/unionc
config_dir=/etc/unionc
package_config="$config_dir/unionc.env"
account_state_dir=/var/lib/unionc-package
managed_user_marker="$account_state_dir/managed-user"
managed_group_marker="$account_state_dir/managed-group"
group_marker_state=absent
user_marker_state=absent
recorded_group_gid=
recorded_user_uid=
recorded_user_primary_gid=
group_created_now=0
group_creation_committed=0
rollback_group_gid=
user_created_now=0
user_creation_committed=0
rollback_user_uid=
rollback_user_primary_gid=

die() {
  echo "unionc postinstall: $*" >&2
  exit 1
}

for command_name in getent groupadd groupdel useradd userdel install cut chown chmod rm mv stat awk; do
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

inspect_existing_markers() {
  if [ -e "$managed_group_marker" ] || [ -L "$managed_group_marker" ]; then
    [ -f "$managed_group_marker" ] && [ ! -L "$managed_group_marker" ] ||
      die "managed group marker is not a safe regular file"
    read_path_metadata "$managed_group_marker"
    [ "$path_uid:$path_gid:$path_mode" = 0:0:600 ] ||
      die "managed group marker must be owned by root:root with permissions 0600"
    load_group_marker || die "managed group marker is not for UnionC $package_version"
    group_marker_state=valid
  fi

  if [ -e "$managed_user_marker" ] || [ -L "$managed_user_marker" ]; then
    [ -f "$managed_user_marker" ] && [ ! -L "$managed_user_marker" ] ||
      die "managed user marker is not a safe regular file"
    read_path_metadata "$managed_user_marker"
    [ "$path_uid:$path_gid:$path_mode" = 0:0:600 ] ||
      die "managed user marker must be owned by root:root with permissions 0600"
    load_user_marker || die "managed user marker is not for UnionC $package_version"
    user_marker_state=valid
  fi

  if [ "$user_marker_state" = valid ] && [ "$group_marker_state" != valid ]; then
    die "managed user marker exists without its managed group marker"
  fi
}

write_group_marker() {
  marker_gid=$1
  marker_temporary="$account_state_dir/.managed-group.$$"
  umask 077
  {
    printf 'format=%s\n' "$package_version"
    printf 'gid=%s\n' "$marker_gid"
  } >"$marker_temporary"
  chown root:root "$marker_temporary"
  chmod 0600 "$marker_temporary"
  mv -f -- "$marker_temporary" "$managed_group_marker"
  group_marker_state=valid
  recorded_group_gid=$marker_gid
}

write_user_marker() {
  marker_uid=$1
  marker_primary_gid=$2
  marker_temporary="$account_state_dir/.managed-user.$$"
  umask 077
  {
    printf 'format=%s\n' "$package_version"
    printf 'uid=%s\n' "$marker_uid"
    printf 'primary_gid=%s\n' "$marker_primary_gid"
  } >"$marker_temporary"
  chown root:root "$marker_temporary"
  chmod 0600 "$marker_temporary"
  mv -f -- "$marker_temporary" "$managed_user_marker"
  user_marker_state=valid
  recorded_user_uid=$marker_uid
  recorded_user_primary_gid=$marker_primary_gid
}

# Enumerate NSS for both normal creation and rollback. A failed keyed lookup is
# not proof that an account is absent, and deletion must never rely on that
# ambiguity after a partially completed package transaction.
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

rollback_user_is_exact() {
  current_uid=$(printf '%s\n' "$user_entry" | cut -d: -f3)
  current_gid=$(printf '%s\n' "$user_entry" | cut -d: -f4)
  current_home=$(printf '%s\n' "$user_entry" | cut -d: -f6)
  current_shell=$(printf '%s\n' "$user_entry" | cut -d: -f7)
  [ -n "$rollback_user_uid" ] && [ -n "$rollback_user_primary_gid" ] &&
    [ "$current_uid" = "$rollback_user_uid" ] &&
    [ "$current_gid" = "$rollback_user_primary_gid" ] &&
    [ "$current_home" = "$data_dir" ] &&
    { [ "$current_shell" = /usr/sbin/nologin ] || [ "$current_shell" = /sbin/nologin ]; }
}

# Return 0 when the just-created group is referenced, 1 when it is unused, and
# 2 when usage cannot be established safely.
rollback_group_is_in_use() {
  current_group_gid=$(printf '%s\n' "$group_entry" | cut -d: -f3)
  group_members=$(printf '%s\n' "$group_entry" | cut -d: -f4-)
  [ -n "$rollback_group_gid" ] && [ "$current_group_gid" = "$rollback_group_gid" ] ||
    return 2
  case "$group_members" in
    *:*) return 2 ;;
    '') ;;
    *) return 0 ;;
  esac

  current_passwd_listing=$(getent passwd 2>/dev/null) || return 2
  while IFS=: read -r account_name _password _uid primary_gid _gecos _home _shell; do
    [ -n "$account_name" ] || continue
    if [ "$primary_gid" = "$rollback_group_gid" ]; then
      return 0
    fi
  done <<EOF
$current_passwd_listing
EOF
  return 1
}

rollback_account_creation() {
  rollback_status=$?
  trap - EXIT HUP INT TERM
  set +e
  if [ "$rollback_status" -eq 0 ]; then
    exit 0
  fi

  # Marker publication is the ownership commit point. Delete only an identity
  # created by this invocation that is still exact and has not been committed.
  if [ "$user_created_now" -eq 1 ] && [ "$user_creation_committed" -eq 0 ] &&
    [ "$user_marker_state" != valid ]; then
    user_lookup_status=0
    if lookup_user_entry; then
      if rollback_user_is_exact; then
        userdel unionc ||
          echo "unionc postinstall: could not roll back the incomplete dedicated user" >&2
      else
        echo "unionc postinstall: refusing to roll back a dedicated user whose identity changed" >&2
      fi
    else
      user_lookup_status=$?
      if [ "$user_lookup_status" -eq 2 ]; then
        echo "unionc postinstall: could not enumerate users while rolling back account creation" >&2
      fi
    fi
  fi

  if [ "$group_created_now" -eq 1 ] && [ "$group_creation_committed" -eq 0 ] &&
    [ "$group_marker_state" != valid ]; then
    group_lookup_status=0
    if lookup_group_entry; then
      current_group_gid=$(printf '%s\n' "$group_entry" | cut -d: -f3)
      if [ -z "$rollback_group_gid" ] || [ "$current_group_gid" != "$rollback_group_gid" ]; then
        echo "unionc postinstall: refusing to roll back a dedicated group whose gid changed" >&2
      else
        group_usage_status=0
        if rollback_group_is_in_use; then
          group_usage_status=0
        else
          group_usage_status=$?
        fi
        case "$group_usage_status" in
          1)
            groupdel unionc ||
              echo "unionc postinstall: could not roll back the incomplete dedicated group" >&2
            ;;
          0)
            echo "unionc postinstall: refusing to roll back an incomplete group that is in use" >&2
            ;;
          *)
            echo "unionc postinstall: could not verify incomplete group usage; preserving it" >&2
            ;;
        esac
      fi
    else
      group_lookup_status=$?
      if [ "$group_lookup_status" -eq 2 ]; then
        echo "unionc postinstall: could not enumerate groups while rolling back account creation" >&2
      fi
    fi
  fi
  rm -f -- "$account_state_dir/.managed-user.$$" "$account_state_dir/.managed-group.$$"
  exit "$rollback_status"
}

# The root-owned marker directory binds the current package version to the
# exact numeric service identity. Removing the package deliberately keeps it,
# so only an exact 0.3.2 reinstall can reclaim the retained state. Never
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

trap rollback_account_creation EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

group_lookup_status=0
if lookup_group_entry; then
  group_lookup_status=0
else
  group_lookup_status=$?
  [ "$group_lookup_status" -eq 1 ] ||
    die "dedicated group database is unavailable or ambiguous"
  [ "$group_marker_state" = absent ] ||
    die "package-managed unionc group is missing"
  groupadd --system unionc
  group_created_now=1
  lookup_group_entry || die "new dedicated group could not be enumerated"
fi
group_gid=$(printf '%s\n' "$group_entry" | cut -d: -f3)
rollback_group_gid=$group_gid
case "$group_gid" in
  ''|*[!0-9]*) die "dedicated group has an invalid gid" ;;
esac
if [ "$group_created_now" -eq 1 ]; then
  write_group_marker "$group_gid"
  group_creation_committed=1
elif [ "$group_marker_state" != valid ]; then
  die "existing unionc group has no current $package_version ownership marker"
elif [ "$recorded_group_gid" != "$group_gid" ]; then
  die "package-managed unionc group was replaced with a different gid"
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
  nologin_shell=/usr/sbin/nologin
  if [ ! -x "$nologin_shell" ]; then
    nologin_shell=/sbin/nologin
  fi
  [ -x "$nologin_shell" ] || die "neither /usr/sbin/nologin nor /sbin/nologin exists"
  useradd --system --gid unionc --home-dir "$data_dir" \
    --shell "$nologin_shell" unionc
  user_created_now=1
  lookup_user_entry || die "new dedicated user could not be enumerated"
fi

user_uid=$(printf '%s\n' "$user_entry" | cut -d: -f3)
user_gid=$(printf '%s\n' "$user_entry" | cut -d: -f4)
rollback_user_uid=$user_uid
rollback_user_primary_gid=$user_gid
user_home=$(printf '%s\n' "$user_entry" | cut -d: -f6)
user_shell=$(printf '%s\n' "$user_entry" | cut -d: -f7)
case "$user_uid:$user_gid" in
  *[!0-9:]*) die "dedicated account has an invalid numeric identity" ;;
  :*|*:) die "dedicated account has a missing numeric identity" ;;
esac
[ "$user_gid" = "$group_gid" ] || die "unionc user does not use the dedicated group"
[ "$user_home" = "$data_dir" ] || die "unionc user has an unexpected home"
case "$user_shell" in
  /usr/sbin/nologin|/sbin/nologin) ;;
  *) die "unionc user has an interactive or unexpected shell" ;;
esac

if [ "$user_created_now" -eq 1 ]; then
  write_user_marker "$user_uid" "$user_gid"
  user_creation_committed=1
elif [ "$user_marker_state" != valid ]; then
  die "existing unionc user has no current $package_version ownership marker"
elif
  { [ "$recorded_user_uid" != "$user_uid" ] ||
    [ "$recorded_user_primary_gid" != "$user_gid" ]; }; then
  die "package-managed unionc user was replaced with a different numeric identity"
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
if [ -d /run/systemd/system ] && systemctl is-enabled --quiet "$service_name"; then
  if ! systemctl restart "$service_name"; then
    echo "警告：UnionC 包已重新安装并保持 enabled，但服务重启失败；请检查 systemctl status unionc 与 journalctl -u unionc。" >&2
  fi
elif [ ! -e "$data_dir/unionc.db" ]; then
  cat <<'EOF'

UnionC 已安装。首次启动前请完成：

  1. 编辑 /etc/unionc/unionc.env，分别填入两个不可复用的随机值
       openssl rand -base64 32        # UNIONC_SECRET_KEY
       openssl rand -hex 32           # UNIONC_PROXY_SECRET
     并把同一个 UNIONC_PROXY_SECRET 安全地配置到可信反向代理环境。
  2. 首次部署临时打开 UNIONC_ALLOW_BOOTSTRAP=1 与 UNIONC_BOOTSTRAP_PASSWORD
  3. systemctl enable --now unionc
  4. 管理员创建完成后，从 unionc.env 删除上述两个 bootstrap 变量并 restart

内嵌 SQLite 数据库会自动创建为 /var/lib/unionc/unionc.db；无需安装或配置数据库服务。
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
