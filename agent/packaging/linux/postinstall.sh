#!/bin/sh
set -eu

service_name=unionc-agent.service
package_version=0.3.2
account_state_dir=/var/lib/unionc-agent-package
rpm_config_backup="$account_state_dir/config.json.remove-backup"
managed_user_marker="$account_state_dir/managed-user"
managed_group_marker="$account_state_dir/managed-group"
group_marker_state=absent
user_marker_state=absent
recorded_group_gid=
recorded_user_uid=
recorded_user_primary_gid=

die() {
  echo "unionc-agent postinstall: $*" >&2
  exit 1
}

for command_name in unionc-agent getent groupadd useradd install cut chown chmod cp rm mv; do
  command -v "$command_name" >/dev/null 2>&1 ||
    die "required command is unavailable: $command_name"
done

[ "$(unionc-agent --version)" = "unionc-agent $package_version" ] ||
  die "installed binary version does not match package lifecycle version $package_version"

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
    if load_group_marker; then
      group_marker_state=valid
    else
      die "managed group marker is invalid"
    fi
  fi

  if [ -e "$managed_user_marker" ] || [ -L "$managed_user_marker" ]; then
    [ -f "$managed_user_marker" ] && [ ! -L "$managed_user_marker" ] ||
      die "managed user marker is not a safe regular file"
    if load_user_marker; then
      user_marker_state=valid
    else
      die "managed user marker is invalid"
    fi
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

# This bookkeeping directory is deliberately separate from the Agent-writable
# state directory. It records that the package owns the dedicated account and
# holds an RPM config backup across an erase transaction.
install -d -m 0700 -o root -g root "$account_state_dir"
inspect_existing_markers

group_created_now=0
if ! getent group unionc-agent >/dev/null 2>&1; then
  groupadd --system unionc-agent
  group_created_now=1
fi
group_entry=$(getent group unionc-agent) || die "dedicated group lookup failed"
group_gid=$(printf '%s\n' "$group_entry" | cut -d: -f3)
case "$group_gid" in
  ''|*[!0-9]*) die "dedicated group has an invalid gid" ;;
esac
if [ "$group_created_now" -eq 1 ]; then
  # Record group ownership before creating the user. If useradd later fails,
  # an explicit purge can still identify this installer-created group safely.
  write_group_marker "$group_gid"
elif [ "$group_marker_state" != valid ]; then
  die "existing unionc-agent group has no current $package_version ownership marker"
elif [ "$recorded_group_gid" != "$group_gid" ]; then
  die "package-managed unionc-agent group was replaced with a different gid"
fi

user_created_now=0
if ! getent passwd unionc-agent >/dev/null 2>&1; then
  nologin_shell=/usr/sbin/nologin
  if [ ! -x "$nologin_shell" ]; then
    nologin_shell=/sbin/nologin
  fi
  [ -x "$nologin_shell" ] || die "neither /usr/sbin/nologin nor /sbin/nologin exists"
  useradd --system --gid unionc-agent --home-dir /var/lib/unionc-agent \
    --shell "$nologin_shell" unionc-agent
  user_created_now=1
fi

# Refuse to run the service under an unexpected pre-existing identity. An
# existing account is accepted only when the current 0.3.2 marker binds its
# exact numeric identity.
user_entry=$(getent passwd unionc-agent) || die "dedicated user lookup failed"
user_uid=$(printf '%s\n' "$user_entry" | cut -d: -f3)
user_gid=$(printf '%s\n' "$user_entry" | cut -d: -f4)
user_home=$(printf '%s\n' "$user_entry" | cut -d: -f6)
user_shell=$(printf '%s\n' "$user_entry" | cut -d: -f7)

case "$user_uid:$user_gid" in
  *[!0-9:]*) die "dedicated account has an invalid numeric identity" ;;
  :*|*:) die "dedicated account has a missing numeric identity" ;;
esac
[ "$user_gid" = "$group_gid" ] || die "unionc-agent user does not use the dedicated group"
[ "$user_home" = /var/lib/unionc-agent ] || die "unionc-agent user has an unexpected home"
case "$user_shell" in
  /usr/sbin/nologin|/sbin/nologin) ;;
  *) die "unionc-agent user has an interactive or unexpected shell" ;;
esac

if [ "$user_created_now" -eq 1 ]; then
  write_user_marker "$user_uid" "$user_gid"
elif [ "$user_marker_state" != valid ]; then
  die "existing unionc-agent user has no current $package_version ownership marker"
elif
  { [ "$recorded_user_uid" != "$user_uid" ] ||
    [ "$recorded_user_primary_gid" != "$user_gid" ]; }; then
  die "package-managed unionc-agent user was replaced with a different numeric identity"
fi

install -d -m 0700 -o unionc-agent -g unionc-agent /var/lib/unionc-agent
install -d -m 0750 -o root -g unionc-agent /etc/unionc-agent

# The current package's pre-remove hook protects config across remove and
# same-version reinstall. Consume that private backup before starting service.
if [ -f "$rpm_config_backup" ]; then
  cp -p "$rpm_config_backup" /etc/unionc-agent/config.json
  rm -f "$rpm_config_backup"
fi
if [ -f /etc/unionc-agent/config.json ]; then
  chown root:unionc-agent /etc/unionc-agent/config.json
  chmod 0640 /etc/unionc-agent/config.json
fi

service_started=0
if [ -d /run/systemd/system ]; then
  command -v systemctl >/dev/null 2>&1 || die "systemd is running but systemctl is unavailable"
  systemctl daemon-reload
  systemctl enable "$service_name"
  systemctl restart "$service_name"
  systemctl is-active --quiet "$service_name" || die "$service_name did not remain active"
  service_started=1
fi

if [ "$service_started" -eq 1 ]; then
  cat <<'EOF'

UnionC Agent 服务已启动，但新安装尚未配对，当前不会发送经过授权的遥测。
请在本机发起浏览器配对：

  sudo unionc-agent pair --config /etc/unionc-agent/config.json \
    --server https://unionc.example.com

管理台只生成一次性激活码，不分发软件，也不会接触 Agent 的长期通信 secret。

配对后验证状态（只读，不发送或清理队列）：

  sudo -u unionc-agent unionc-agent status --output human \
    --config /etc/unionc-agent/config.json
  sudo -u unionc-agent unionc-agent doctor --output human \
    --config /etc/unionc-agent/config.json

查看服务和日志：

  systemctl status unionc-agent.service
  journalctl -u unionc-agent.service -n 100 --no-pager

EOF
else
  cat <<'EOF'

UnionC Agent 文件已安装；当前环境没有运行 systemd，因此没有启用或启动后台服务。
进入正常启动的系统后执行：

  sudo systemctl enable --now unionc-agent.service
  sudo unionc-agent pair --config /etc/unionc-agent/config.json \
    --server https://unionc.example.com

配对后使用 `unionc-agent status --output human` 验证授权状态。

EOF
fi

# 默认 unit 设置 PrivateDevices=yes，会屏蔽 /dev/nvidia* 与 /dev/dri。
# 需要 GPU 采集时安装随包分发的 drop-in。
if [ -e /dev/nvidiactl ] || [ -d /dev/dri ]; then
  gpu_groups=""
  for gpu_group in render video; do
    if getent group "$gpu_group" >/dev/null 2>&1; then
      if [ -n "$gpu_groups" ]; then
        gpu_groups="$gpu_groups,$gpu_group"
      else
        gpu_groups="$gpu_group"
      fi
    fi
  done
  cat <<'EOF'

检测到本机存在 GPU 设备节点。默认 unit 出于安全考虑设置了 PrivateDevices=yes，
因此裸 shell 中的 probe 结果不能代表 systemd 服务实际可见的 GPU。
EOF
  if [ -n "$gpu_groups" ]; then
    cat <<EOF

本机存在设备访问组：$gpu_groups。如确需启用 GPU 指标，请显式执行：

  usermod -aG $gpu_groups unionc-agent
  mkdir -p /etc/systemd/system/unionc-agent.service.d
  cp /usr/share/unionc-agent/unionc-agent-gpu.conf \
     /etc/systemd/system/unionc-agent.service.d/gpu.conf
  systemctl daemon-reload && systemctl restart unionc-agent

EOF
  else
    cat <<'EOF'

未找到 render/video 设备访问组，因此没有给出会导致服务启动失败的盲目配置命令。
请先确认设备节点的实际属组，再授予 unionc-agent 最小必要组权限并安装 GPU drop-in。

EOF
  fi
fi
