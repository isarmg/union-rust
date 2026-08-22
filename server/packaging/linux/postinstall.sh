#!/bin/sh
set -eu

PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH

service_name=unionc.service
package_version=0.3.2
server_binary=/usr/bin/unionc
data_dir=/var/lib/unionc
package_config=/etc/unionc/unionc.env
account_state_dir=/var/lib/unionc-package
managed_user_marker="$account_state_dir/managed-user"
managed_group_marker="$account_state_dir/managed-group"
group_marker_state=absent
user_marker_state=absent
recorded_group_gid=
recorded_user_uid=
recorded_user_primary_gid=

die() {
  echo "unionc postinstall: $*" >&2
  exit 1
}

for command_name in getent groupadd useradd install cut chown chmod mv stat awk; do
  command -v "$command_name" >/dev/null 2>&1 ||
    die "required command is unavailable: $command_name"
done
[ -x "$server_binary" ] || die "installed Server binary is missing or not executable"

[ "$("$server_binary" --version)" = "unionc $package_version" ] ||
  die "installed binary version does not match package lifecycle version $package_version"

# nFPM lays down this config before invoking postinstall. Its exact marker
# distinguishes the current package payload from a retained or unrelated file
# before this hook creates an account, marker directory, or data directory.
[ -f "$package_config" ] && [ ! -L "$package_config" ] ||
  die "$package_config is not a safe current-package config file"
package_config_marker=$(
  awk -v expected="UNIONC_PACKAGE_VERSION=$package_version" '
    /^[[:space:]]*(export[[:space:]]+)?UNIONC_PACKAGE_VERSION[[:space:]]*=/ {
      seen += 1
      if ($0 == expected) valid += 1
    }
    END { printf "%d:%d", seen, valid }
  ' "$package_config"
)
[ "$package_config_marker" = 1:1 ] ||
  die "$package_config must contain exactly one current UNIONC_PACKAGE_VERSION=$package_version marker"

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
    load_group_marker || die "managed group marker is not for UnionC $package_version"
    group_marker_state=valid
  fi

  if [ -e "$managed_user_marker" ] || [ -L "$managed_user_marker" ]; then
    [ -f "$managed_user_marker" ] && [ ! -L "$managed_user_marker" ] ||
      die "managed user marker is not a safe regular file"
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

# The root-owned marker directory binds the current package version to the
# exact numeric service identity. Removing the package deliberately keeps it,
# so only an exact 0.3.2 reinstall can reclaim the retained state.
if [ -e "$account_state_dir" ] || [ -L "$account_state_dir" ]; then
  [ -d "$account_state_dir" ] && [ ! -L "$account_state_dir" ] ||
    die "package account state path is not a safe directory"
fi
install -d -m 0700 -o root -g root "$account_state_dir"
inspect_existing_markers

data_dir_preexisting=0
if [ -e "$data_dir" ] || [ -L "$data_dir" ]; then
  [ -d "$data_dir" ] && [ ! -L "$data_dir" ] ||
    die "$data_dir is not a safe directory"
  data_dir_preexisting=1
  [ "$group_marker_state" = valid ] && [ "$user_marker_state" = valid ] ||
    die "refusing to adopt pre-existing $data_dir without current package ownership markers"
fi

group_created_now=0
if ! getent group unionc >/dev/null 2>&1; then
  [ "$group_marker_state" = absent ] ||
    die "package-managed unionc group is missing"
  groupadd --system unionc
  group_created_now=1
fi
group_entry=$(getent group unionc) || die "dedicated group lookup failed"
group_gid=$(printf '%s\n' "$group_entry" | cut -d: -f3)
case "$group_gid" in
  ''|*[!0-9]*) die "dedicated group has an invalid gid" ;;
esac
if [ "$group_created_now" -eq 1 ]; then
  write_group_marker "$group_gid"
elif [ "$group_marker_state" != valid ]; then
  die "existing unionc group has no current $package_version ownership marker"
elif [ "$recorded_group_gid" != "$group_gid" ]; then
  die "package-managed unionc group was replaced with a different gid"
fi

user_created_now=0
if ! getent passwd unionc >/dev/null 2>&1; then
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
fi

user_entry=$(getent passwd unionc) || die "dedicated user lookup failed"
user_uid=$(printf '%s\n' "$user_entry" | cut -d: -f3)
user_gid=$(printf '%s\n' "$user_entry" | cut -d: -f4)
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

chown root:unionc "$package_config"
chmod 0640 "$package_config"

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
