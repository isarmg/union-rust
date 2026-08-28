#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
packaging_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)
test_root=$(mktemp -d "${TMPDIR:-/tmp}/unionc-agent-packaging-test.XXXXXX")
package_version=0.5.0
workspace_version=$(sed -n 's/^version = "\([0-9][0-9.]*\)"$/\1/p' "$packaging_dir/../../../Cargo.toml")
[ "$workspace_version" = "$package_version" ] || {
  echo "Linux ownership-marker version must follow the current Cargo package version" >&2
  exit 1
}

cleanup() {
  case "$test_root" in
    "${TMPDIR:-/tmp}"/unionc-agent-packaging-test.*)
      rm -rf -- "$test_root"
      ;;
    *)
      echo "refusing to remove unexpected test path: $test_root" >&2
      ;;
  esac
}
trap cleanup EXIT HUP INT TERM

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

assert_exists() {
  [ -e "$1" ] || fail "expected path to exist: $1"
}

assert_absent() {
  [ ! -e "$1" ] || fail "expected path to be absent: $1"
}

assert_log_contains() {
  grep -F -- "$1" "$TEST_LOG" >/dev/null || fail "log does not contain: $1"
}

rewrite_for_test_root() {
  source_file=$1
  destination_file=$2
  sed \
    -e 's#/var/lib/unionc-agent-package#__PACKAGE_STATE__#g' \
    -e 's#/var/lib/unionc-agent#__AGENT_STATE__#g' \
    -e 's#/etc/systemd/system/unionc-agent.service.d#__DROPIN_DIR__#g' \
    -e 's#/etc/unionc-agent#__CONFIG_DIR__#g' \
    -e 's#/run/systemd/system#__SYSTEMD_RUNTIME__#g' \
    -e 's#/usr/bin/unionc-agent#__TEST_AGENT_BINARY__#g' \
    -e 's#^PATH=/usr/sbin:/usr/bin:/sbin:/bin$#PATH=__TEST_BIN__:/usr/sbin:/usr/bin:/sbin:/bin#' \
    -e "s#__PACKAGE_STATE__#$test_root/var/lib/unionc-agent-package#g" \
    -e "s#__AGENT_STATE__#$test_root/var/lib/unionc-agent#g" \
    -e "s#__DROPIN_DIR__#$test_root/etc/systemd/system/unionc-agent.service.d#g" \
    -e "s#__CONFIG_DIR__#$test_root/etc/unionc-agent#g" \
    -e "s#__SYSTEMD_RUNTIME__#$test_root/run/systemd/system#g" \
    -e "s#__TEST_AGENT_BINARY__#$test_root/bin/unionc-agent#g" \
    -e "s#__TEST_BIN__#$test_root/bin#g" \
    "$source_file" >"$destination_file"
  chmod 0755 "$destination_file"
}

for source_script in \
  "$packaging_dir/postinstall.sh" \
  "$packaging_dir/preremove.sh" \
  "$packaging_dir/postremove.sh" \
  "$packaging_dir/purge-local-state.sh"
do
  sh -n "$source_script"
  grep -Fx 'PATH=/usr/sbin:/usr/bin:/sbin:/bin' "$source_script" >/dev/null ||
    fail "root lifecycle script does not replace the caller PATH: $source_script"
  rewrite_for_test_root "$source_script" "$test_root/$(basename "$source_script")"
done

grep -Fx 'Type=notify' "$packaging_dir/unionc-agent.service" >/dev/null ||
  fail 'systemd unit does not wait for Agent readiness'
grep -Fx 'NotifyAccess=main' "$packaging_dir/unionc-agent.service" >/dev/null ||
  fail 'systemd unit accepts readiness from an unexpected process'
grep -Fx 'TimeoutStartSec=30s' "$packaging_dir/unionc-agent.service" >/dev/null ||
  fail 'systemd unit has no bounded readiness timeout'
if grep -Fx 'Type=simple' "$packaging_dir/unionc-agent.service" >/dev/null; then
  fail 'systemd unit can report startup before Agent initialization'
fi

mkdir -p "$test_root/bin" "$test_root/run/systemd/system"
TEST_LOG="$test_root/commands.log"
export TEST_LOG test_root
: >"$TEST_LOG"

cat >"$test_root/bin/systemctl" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >>"$TEST_LOG"
case "$1" in
  show)
    case "$*" in
      *--property=LoadState*)
        [ "${FAIL_SYSTEMCTL_LOAD_QUERY:-0}" -ne 1 ] || exit 70
        [ "${EMPTY_SYSTEMCTL_LOAD_QUERY:-0}" -ne 1 ] || exit 0
        printf '%s\n' "${SYSTEMCTL_LOAD_STATE:-loaded}"
        ;;
      *--property=ActiveState*)
        [ "${FAIL_SYSTEMCTL_ACTIVE_QUERY:-0}" -ne 1 ] || exit 71
        [ "${EMPTY_SYSTEMCTL_ACTIVE_QUERY:-0}" -ne 1 ] || exit 0
        printf '%s\n' "${SYSTEMCTL_ACTIVE_STATE:-inactive}"
        ;;
      *) exit 72 ;;
    esac
    ;;
  restart)
    [ "${FAIL_RESTART:-0}" -ne 1 ]
    ;;
  is-active)
    [ "${FAIL_ACTIVE:-0}" -ne 1 ]
    ;;
  *)
    exit 0
    ;;
esac
EOF

cat >"$test_root/bin/getent" <<'EOF'
#!/bin/sh
current_agent_uid=${AGENT_UID:-998}
current_agent_gid=${AGENT_GID:-998}
if [ -f "$test_root/current-user-uid" ]; then
  IFS= read -r current_agent_uid <"$test_root/current-user-uid"
fi
if [ -f "$test_root/current-group-gid" ]; then
  IFS= read -r current_agent_gid <"$test_root/current-group-gid"
fi
case "$1:$2" in
  passwd:unionc-agent)
    if [ "${START_ACCOUNTS_ABSENT:-0}" -eq 1 ] && [ ! -f "$test_root/user.created" ]; then
      exit 2
    fi
    [ ! -f "$test_root/user.deleted" ] || exit 2
    echo "unionc-agent:x:$current_agent_uid:$current_agent_gid::TEST_AGENT_STATE:/usr/sbin/nologin" |
      sed "s#TEST_AGENT_STATE#$test_root/var/lib/unionc-agent#"
    ;;
  passwd:)
    [ "${FAIL_PASSWD_ENUM:-0}" -ne 1 ] || exit 2
    if { [ "${START_ACCOUNTS_ABSENT:-0}" -ne 1 ] || [ -f "$test_root/user.created" ]; } &&
      [ ! -f "$test_root/user.deleted" ]; then
      echo "unionc-agent:x:$current_agent_uid:$current_agent_gid::TEST_AGENT_STATE:/usr/sbin/nologin" |
        sed "s#TEST_AGENT_STATE#$test_root/var/lib/unionc-agent#"
    fi
    if [ -n "${OTHER_PRIMARY_GID:-}" ]; then
      echo "other-user:x:1500:${OTHER_PRIMARY_GID}:Other:/nonexistent:/usr/sbin/nologin"
    fi
    ;;
  group:unionc-agent)
    if [ "${START_ACCOUNTS_ABSENT:-0}" -eq 1 ] && [ ! -f "$test_root/group.created" ]; then
      exit 2
    fi
    [ ! -f "$test_root/group.deleted" ] || exit 2
    echo "unionc-agent:x:$current_agent_gid:${SUPPLEMENTARY_MEMBER:-}"
    ;;
  group:)
    [ "${FAIL_GROUP_ENUM:-0}" -ne 1 ] || exit 2
    if { [ "${START_ACCOUNTS_ABSENT:-0}" -ne 1 ] || [ -f "$test_root/group.created" ]; } &&
      [ ! -f "$test_root/group.deleted" ]; then
      echo "unionc-agent:x:$current_agent_gid:${SUPPLEMENTARY_MEMBER:-}"
    fi
    ;;
  *)
    exit 2
    ;;
esac
EOF

cat >"$test_root/bin/groupadd" <<'EOF'
#!/bin/sh
[ "${START_ACCOUNTS_ABSENT:-0}" -eq 1 ]
rm -f "$test_root/group.deleted"
: >"$test_root/group.created"
printf '%s\n' "groupadd $*" >>"$TEST_LOG"
EOF

cat >"$test_root/bin/useradd" <<'EOF'
#!/bin/sh
[ "${START_ACCOUNTS_ABSENT:-0}" -eq 1 ]
rm -f "$test_root/user.deleted"
: >"$test_root/user.created"
printf '%s\n' "useradd $*" >>"$TEST_LOG"
EOF

cat >"$test_root/bin/userdel" <<'EOF'
#!/bin/sh
[ "$1" = unionc-agent ]
: >"$test_root/user.deleted"
printf '%s\n' "userdel $*" >>"$TEST_LOG"
EOF

cat >"$test_root/bin/groupdel" <<'EOF'
#!/bin/sh
[ "$1" = unionc-agent ]
: >"$test_root/group.deleted"
printf '%s\n' "groupdel $*" >>"$TEST_LOG"
EOF

cat >"$test_root/bin/install" <<'EOF'
#!/bin/sh
destination=
for argument in "$@"; do
  destination=$argument
done
[ -n "$destination" ]
mkdir -p "$destination"
EOF

cat >"$test_root/bin/chown" <<'EOF'
#!/bin/sh
owner=${1:-}
destination=
for argument in "$@"; do
  destination=$argument
done
case "$owner:$destination" in
  "root:root:$test_root/var/lib/unionc-agent-package/config.json.remove-backup")
    : >"$test_root/backup.root-owned"
    ;;
  "root:root:$test_root/var/lib/unionc-agent-package/.config.json.remove-backup."*)
    : >"$test_root/backup.root-owned"
    : >"$test_root/backup-temporary.root-owned"
    ;;
esac
case "$destination" in
  "$test_root/etc/unionc-agent/.config.json.restore."*)
    case "$owner" in
      root:*)
        restore_group=${owner#root:}
        case "$restore_group" in
          ''|*[!0-9]*) ;;
          *)
            restore_metadata_key=${destination##*/}
            printf '%s\n' "$restore_group" >"$test_root/$restore_metadata_key.gid"
            ;;
        esac
        ;;
    esac
    ;;
esac
exit 0
EOF

cat >"$test_root/bin/mv" <<'EOF'
#!/bin/sh
source_path=
destination=
for argument in "$@"; do
  source_path=$destination
  destination=$argument
done
case "$destination" in
  "$test_root/var/lib/unionc-agent-package/managed-group")
    if [ "${FAIL_GROUP_MARKER_MOVE:-0}" -eq 1 ]; then
      if [ "${REPLACE_GROUP_BEFORE_ROLLBACK:-0}" -eq 1 ]; then
        printf '997\n' >"$test_root/current-group-gid"
      fi
      exit 73
    fi
    ;;
  "$test_root/var/lib/unionc-agent-package/managed-user")
    if [ "${FAIL_USER_MARKER_MOVE:-0}" -eq 1 ]; then
      if [ "${REPLACE_USER_BEFORE_ROLLBACK:-0}" -eq 1 ]; then
        printf '997\n' >"$test_root/current-user-uid"
      fi
      exit 74
    fi
    ;;
  "$test_root/var/lib/unionc-agent-package/config.json.remove-backup")
    [ "${FAIL_BACKUP_MOVE:-0}" -ne 1 ] || exit 75
    ;;
  "$test_root/etc/unionc-agent/config.json")
    case "$source_path" in
      "$test_root/etc/unionc-agent/.config.json.restore."*)
        [ "${FAIL_RESTORE_MOVE:-0}" -ne 1 ] || exit 76
        ;;
    esac
    ;;
esac
exec /usr/bin/mv "$@"
EOF

cat >"$test_root/bin/cp" <<'EOF'
#!/bin/sh
destination=
for argument in "$@"; do
  destination=$argument
done
case "$destination" in
  "$test_root/var/lib/unionc-agent-package/.config.json.remove-backup."*)
    [ "${FAIL_BACKUP_COPY:-0}" -ne 1 ] || exit 77
    ;;
  "$test_root/etc/unionc-agent/.config.json.restore."*)
    [ "${FAIL_RESTORE_COPY:-0}" -ne 1 ] || exit 78
    ;;
esac
exec /usr/bin/cp "$@"
EOF

cat >"$test_root/bin/stat" <<'EOF'
#!/bin/sh
format=
path=
while [ "$#" -gt 0 ]; do
  case "$1" in
    -c)
      shift
      format=$1
      ;;
    --) ;;
    *) path=$1 ;;
  esac
  shift
done

case "$path" in
  "$test_root/var/lib/unionc-agent-package")
    metadata=${STAT_ACCOUNT_STATE:-0:0:700}
    ;;
  "$test_root/var/lib/unionc-agent-package/managed-user")
    metadata=${STAT_MANAGED_USER:-0:0:600}
    ;;
  "$test_root/var/lib/unionc-agent-package/managed-group")
    metadata=${STAT_MANAGED_GROUP:-0:0:600}
    ;;
  "$test_root/var/lib/unionc-agent")
    metadata=${STAT_AGENT_STATE:-${AGENT_UID:-998}:${AGENT_GID:-998}:700}
    ;;
  "$test_root/etc/unionc-agent")
    if [ -f "$test_root/var/lib/unionc-agent-package/managed-group" ]; then
      metadata=${STAT_CONFIG_DIR:-0:${AGENT_GID:-998}:750}
    else
      metadata=${STAT_CONFIG_DIR:-0:0:755}
    fi
    ;;
  "$test_root/var/lib/unionc-agent-package/.config.json.remove-backup."*)
    if [ -f "$test_root/backup-temporary.root-owned" ]; then
      backup_mode=$(/usr/bin/stat -c %a -- "$path")
      metadata=${STAT_CONFIG_BACKUP_TEMPORARY:-0:0:$backup_mode}
    else
      metadata=${STAT_CONFIG_BACKUP_TEMPORARY:-1000:1000:640}
    fi
    ;;
  "$test_root/etc/unionc-agent/config.json")
    if [ -f "$test_root/var/lib/unionc-agent-package/managed-group" ]; then
      metadata=${STAT_CONFIG_FILE:-0:${AGENT_GID:-998}:640}
    else
      metadata=${STAT_CONFIG_FILE:-0:0:600}
    fi
    ;;
  "$test_root/etc/unionc-agent/.config.json.restore."*)
    restore_mode=$(/usr/bin/stat -c %a -- "$path")
    restore_metadata_key=${path##*/}
    if [ -f "$test_root/$restore_metadata_key.gid" ]; then
      IFS= read -r restore_group <"$test_root/$restore_metadata_key.gid"
      metadata=${STAT_CONFIG_RESTORE:-0:$restore_group:$restore_mode}
    else
      metadata=${STAT_CONFIG_RESTORE:-1000:1000:$restore_mode}
    fi
    ;;
  "$test_root/var/lib/unionc-agent-package/config.json.remove-backup")
    if [ -f "$test_root/backup.root-owned" ]; then
      backup_mode=$(/usr/bin/stat -c %a -- "$path")
      metadata=${STAT_CONFIG_BACKUP:-0:0:$backup_mode}
    else
      metadata=${STAT_CONFIG_BACKUP:-1000:1000:640}
    fi
    ;;
  *)
    exec /usr/bin/stat -c "$format" -- "$path"
    ;;
esac

uid=${metadata%%:*}
remainder=${metadata#*:}
gid=${remainder%%:*}
mode=${remainder#*:}
case "$format" in
  %u) printf '%s\n' "$uid" ;;
  %g) printf '%s\n' "$gid" ;;
  %a) printf '%s\n' "$mode" ;;
  %u:%g:%a) printf '%s:%s:%s\n' "$uid" "$gid" "$mode" ;;
  *) exit 2 ;;
esac
EOF

cat >"$test_root/bin/id" <<'EOF'
#!/bin/sh
[ "${1:-}" = -u ] && echo 0
EOF

cat >"$test_root/bin/unionc-agent" <<EOF
#!/bin/sh
printf 'unionc-agent %s\n' '$package_version'
EOF

chmod 0755 "$test_root/bin/"*
PATH="$test_root/bin:$PATH"
export PATH

write_account_markers() {
  marker_uid=${1:-998}
  marker_user_gid=${2:-998}
  marker_group_gid=${3:-998}
  mkdir -p "$test_root/var/lib/unionc-agent-package"
  {
    printf 'format=%s\n' "$package_version"
    printf 'uid=%s\n' "$marker_uid"
    printf 'primary_gid=%s\n' "$marker_user_gid"
  } >"$test_root/var/lib/unionc-agent-package/managed-user"
  {
    printf 'format=%s\n' "$package_version"
    printf 'gid=%s\n' "$marker_group_gid"
  } >"$test_root/var/lib/unionc-agent-package/managed-group"
}

write_package_config() {
  mkdir -p "$test_root/etc/unionc-agent"
  {
    printf '{\n'
    printf '  "application_version": "%s",\n' "$package_version"
    printf '  "server_url": null\n'
    printf '}\n'
  } >"$test_root/etc/unionc-agent/config.json"
}

reset_safe_reinstall_state() {
  rm -rf -- \
    "$test_root/var/lib/unionc-agent-package" \
    "$test_root/var/lib/unionc-agent" \
    "$test_root/etc/unionc-agent"
  rm -f -- \
    "$test_root/user.created" "$test_root/group.created" \
    "$test_root/user.deleted" "$test_root/group.deleted" \
    "$test_root/backup.root-owned" "$test_root/backup-temporary.root-owned" \
    "$test_root/current-user-uid" \
    "$test_root/current-group-gid"
  write_account_markers
  mkdir -p "$test_root/var/lib/unionc-agent"
  write_package_config
}

reset_fresh_install_state() {
  rm -rf -- \
    "$test_root/var/lib/unionc-agent-package" \
    "$test_root/var/lib/unionc-agent" \
    "$test_root/etc/unionc-agent"
  rm -f -- \
    "$test_root/user.created" "$test_root/group.created" \
    "$test_root/user.deleted" "$test_root/group.deleted" \
    "$test_root/backup.root-owned" "$test_root/backup-temporary.root-owned" \
    "$test_root/current-user-uid" \
    "$test_root/current-group-gid"
  write_package_config
}

write_trusted_rpm_backup() {
  /usr/bin/cp "$test_root/etc/unionc-agent/config.json" \
    "$test_root/var/lib/unionc-agent-package/config.json.remove-backup"
  chmod 0600 "$test_root/var/lib/unionc-agent-package/config.json.remove-backup"
  : >"$test_root/backup.root-owned"
}

assert_no_backup_temporary() {
  for temporary_path in \
    "$test_root/var/lib/unionc-agent-package"/.config.json.remove-backup.*
  do
    if [ -e "$temporary_path" ] || [ -L "$temporary_path" ]; then
      fail "unexpected RPM backup temporary remains: $temporary_path"
    fi
  done
}

assert_no_restore_temporary() {
  for temporary_path in "$test_root/etc/unionc-agent"/.config.json.restore.*; do
    if [ -e "$temporary_path" ] || [ -L "$temporary_path" ]; then
      fail "unexpected RPM restore temporary remains: $temporary_path"
    fi
  done
}

# Package managers invoke these scripts as root, but their inherited PATH is
# not an authority boundary. A same-name executable supplied by the caller
# must never replace the packaged Agent or account/system utilities.
reset_safe_reinstall_state
mkdir -p "$test_root/attacker-bin"
for attacker_command in unionc-agent getent systemctl stat; do
  cat >"$test_root/attacker-bin/$attacker_command" <<'EOF'
#!/bin/sh
: >"$test_root/attacker-command-ran"
exit 99
EOF
  chmod 0755 "$test_root/attacker-bin/$attacker_command"
done
rm -f "$test_root/attacker-command-ran"
PATH="$test_root/attacker-bin" "$test_root/postinstall.sh" \
  >"$test_root/safe-path.log" 2>&1 || fail 'postinstall failed with an untrusted caller PATH'
assert_absent "$test_root/attacker-command-ran"

# Debian exposes same-version reinstall through its `upgrade` script ABI. The
# current package is accepted, while a different package version fails closed.
: >"$TEST_LOG"
"$test_root/preremove.sh" upgrade "$package_version"
assert_log_contains 'stop unionc-agent.service'
if "$test_root/preremove.sh" upgrade 0.3.1 >/dev/null 2>&1; then
  fail 'Debian cross-version replacement was accepted'
fi

# RPM replacement runs pre-remove after current postinstall. A
# positive remaining-instance count must not disable the validated 0.5.0 service.
: >"$TEST_LOG"
"$test_root/preremove.sh" 1
[ ! -s "$TEST_LOG" ] || fail 'RPM same-version reinstall stopped the current service'

# Current remove/reinstall protects the config before payload removal and
# restores it afterwards.
mkdir -p "$test_root/etc/unionc-agent" \
  "$test_root/var/lib/unionc-agent" \
  "$test_root/etc/systemd/system/unionc-agent.service.d"
write_package_config
sed -i 's/"server_url": null/"server_url": "retained-config"/' \
  "$test_root/etc/unionc-agent/config.json"
echo retained-state >"$test_root/var/lib/unionc-agent/agent-token"
echo retained-dropin >"$test_root/etc/systemd/system/unionc-agent.service.d/gpu.conf"
: >"$TEST_LOG"
"$test_root/preremove.sh" 0
assert_exists "$test_root/var/lib/unionc-agent-package/config.json.remove-backup"
[ ! -L "$test_root/var/lib/unionc-agent-package/config.json.remove-backup" ] ||
  fail 'RPM config backup was published as a symlink'
[ "$(stat -c '%u:%g:%a' -- "$test_root/var/lib/unionc-agent-package/config.json.remove-backup")" = 0:0:600 ] ||
  fail 'RPM config backup was not published as root:root 0600'
assert_log_contains 'disable --now unionc-agent.service'
rm -f "$test_root/etc/unionc-agent/config.json"
"$test_root/postremove.sh" 0
assert_exists "$test_root/etc/unionc-agent/config.json"
grep -F retained-config "$test_root/etc/unionc-agent/config.json" >/dev/null ||
  fail 'RPM config content was not preserved'
assert_exists "$test_root/var/lib/unionc-agent/agent-token"
assert_exists "$test_root/etc/systemd/system/unionc-agent.service.d/gpu.conf"
assert_log_contains 'daemon-reload'

# The RPM erase backup is a privileged transaction. Neither its source,
# private bookkeeping root, final backup, nor restore destination may be
# redirected or made writable by another identity.
reset_safe_reinstall_state
rm -rf -- "$test_root/foreign-rpm-bookkeeping-pre"
mv "$test_root/var/lib/unionc-agent-package" "$test_root/foreign-rpm-bookkeeping-pre"
: >"$test_root/foreign-rpm-bookkeeping-pre/sentinel"
ln -s "$test_root/foreign-rpm-bookkeeping-pre" \
  "$test_root/var/lib/unionc-agent-package"
if "$test_root/preremove.sh" 0 >"$test_root/rpm-backup-parent-symlink.log" 2>&1; then
  fail 'RPM preremove followed a symlinked bookkeeping directory'
fi
assert_exists "$test_root/foreign-rpm-bookkeeping-pre/sentinel"
assert_absent "$test_root/foreign-rpm-bookkeeping-pre/config.json.remove-backup"

reset_safe_reinstall_state
if STAT_ACCOUNT_STATE=0:0:777 "$test_root/preremove.sh" 0 \
  >"$test_root/rpm-backup-open-parent.log" 2>&1; then
  fail 'RPM preremove trusted a world-writable bookkeeping directory'
fi
assert_absent "$test_root/var/lib/unionc-agent-package/config.json.remove-backup"

reset_safe_reinstall_state
if STAT_MANAGED_USER=0:0:666 "$test_root/preremove.sh" 0 \
  >"$test_root/rpm-backup-open-marker.log" 2>&1; then
  fail 'RPM preremove trusted a writable ownership marker'
fi
assert_absent "$test_root/var/lib/unionc-agent-package/config.json.remove-backup"

reset_safe_reinstall_state
if FAIL_BACKUP_COPY=1 "$test_root/preremove.sh" 0 \
  >"$test_root/rpm-backup-copy-failure.log" 2>&1; then
  fail 'RPM preremove accepted a failed backup copy'
fi
assert_absent "$test_root/var/lib/unionc-agent-package/config.json.remove-backup"
assert_no_backup_temporary

reset_safe_reinstall_state
if STAT_CONFIG_BACKUP_TEMPORARY=0:0:640 "$test_root/preremove.sh" 0 \
  >"$test_root/rpm-backup-temporary-mode.log" 2>&1; then
  fail 'RPM preremove published a backup temporary with unsafe metadata'
fi
assert_absent "$test_root/var/lib/unionc-agent-package/config.json.remove-backup"
assert_no_backup_temporary

reset_safe_reinstall_state
rm -f -- "$test_root/foreign-rpm-config-file"
mv "$test_root/etc/unionc-agent/config.json" "$test_root/foreign-rpm-config-file"
ln -s "$test_root/foreign-rpm-config-file" "$test_root/etc/unionc-agent/config.json"
if "$test_root/preremove.sh" 0 >"$test_root/rpm-backup-config-symlink.log" 2>&1; then
  fail 'RPM preremove followed a symlinked config file'
fi
assert_exists "$test_root/foreign-rpm-config-file"
assert_absent "$test_root/var/lib/unionc-agent-package/config.json.remove-backup"

reset_safe_reinstall_state
rm -rf -- "$test_root/foreign-rpm-config-dir"
mv "$test_root/etc/unionc-agent" "$test_root/foreign-rpm-config-dir"
ln -s "$test_root/foreign-rpm-config-dir" "$test_root/etc/unionc-agent"
if "$test_root/preremove.sh" 0 >"$test_root/rpm-backup-config-dir-symlink.log" 2>&1; then
  fail 'RPM preremove followed a symlinked config directory'
fi
assert_exists "$test_root/foreign-rpm-config-dir/config.json"
assert_absent "$test_root/var/lib/unionc-agent-package/config.json.remove-backup"

reset_safe_reinstall_state
rm -f -- "$test_root/foreign-rpm-backup-target"
/usr/bin/cp "$test_root/etc/unionc-agent/config.json" "$test_root/foreign-rpm-backup-target"
ln -s "$test_root/foreign-rpm-backup-target" \
  "$test_root/var/lib/unionc-agent-package/config.json.remove-backup"
if "$test_root/preremove.sh" 0 >"$test_root/rpm-backup-final-symlink.log" 2>&1; then
  fail 'RPM preremove followed a pre-existing backup symlink'
fi
assert_exists "$test_root/foreign-rpm-backup-target"
[ ! -L "$test_root/foreign-rpm-backup-target" ] ||
  fail 'foreign RPM backup target changed type'

# Atomic publication keeps the previous valid backup byte-for-byte when the
# final rename fails, and its EXIT cleanup removes only the private temporary.
reset_safe_reinstall_state
sed -i 's/"server_url": null/"server_url": "old-backup"/' \
  "$test_root/etc/unionc-agent/config.json"
write_trusted_rpm_backup
sed -i 's/"server_url": "old-backup"/"server_url": "new-backup"/' \
  "$test_root/etc/unionc-agent/config.json"
if FAIL_BACKUP_MOVE=1 "$test_root/preremove.sh" 0 \
  >"$test_root/rpm-backup-atomic-move.log" 2>&1; then
  fail 'RPM preremove accepted a failed atomic backup publication'
fi
grep -F '"server_url": "old-backup"' \
  "$test_root/var/lib/unionc-agent-package/config.json.remove-backup" >/dev/null ||
  fail 'failed RPM backup publication replaced the previous backup'
assert_no_backup_temporary

# Restore validates the source before even probing children through an unsafe
# parent, so a foreign backup remains untouched and cannot become root config.
reset_safe_reinstall_state
sed -i 's/"server_url": null/"server_url": "foreign-parent-backup"/' \
  "$test_root/etc/unionc-agent/config.json"
write_trusted_rpm_backup
rm -f "$test_root/etc/unionc-agent/config.json"
rm -rf -- "$test_root/foreign-rpm-bookkeeping-restore"
mv "$test_root/var/lib/unionc-agent-package" \
  "$test_root/foreign-rpm-bookkeeping-restore"
: >"$test_root/foreign-rpm-bookkeeping-restore/sentinel"
ln -s "$test_root/foreign-rpm-bookkeeping-restore" \
  "$test_root/var/lib/unionc-agent-package"
if "$test_root/postremove.sh" 0 >"$test_root/rpm-restore-parent-symlink.log" 2>&1; then
  fail 'RPM postremove restored through a symlinked bookkeeping directory'
fi
assert_exists "$test_root/foreign-rpm-bookkeeping-restore/sentinel"
assert_exists "$test_root/foreign-rpm-bookkeeping-restore/config.json.remove-backup"
assert_absent "$test_root/etc/unionc-agent/config.json"

reset_safe_reinstall_state
rm -f -- "$test_root/foreign-rpm-restore-source"
/usr/bin/cp "$test_root/etc/unionc-agent/config.json" "$test_root/foreign-rpm-restore-source"
chmod 0600 "$test_root/foreign-rpm-restore-source"
ln -s "$test_root/foreign-rpm-restore-source" \
  "$test_root/var/lib/unionc-agent-package/config.json.remove-backup"
rm -f "$test_root/etc/unionc-agent/config.json"
if "$test_root/postremove.sh" 0 >"$test_root/rpm-restore-backup-symlink.log" 2>&1; then
  fail 'RPM postremove followed a backup symlink'
fi
assert_exists "$test_root/foreign-rpm-restore-source"
assert_absent "$test_root/etc/unionc-agent/config.json"

reset_safe_reinstall_state
write_trusted_rpm_backup
rm -f "$test_root/etc/unionc-agent/config.json"
if STAT_CONFIG_BACKUP=1000:1000:600 "$test_root/postremove.sh" 0 \
  >"$test_root/rpm-restore-backup-owner.log" 2>&1; then
  fail 'RPM postremove trusted a foreign-owned config backup'
fi
assert_exists "$test_root/var/lib/unionc-agent-package/config.json.remove-backup"
assert_absent "$test_root/etc/unionc-agent/config.json"

reset_safe_reinstall_state
sed -i 's/"server_url": null/"server_url": "copy-failure-backup"/' \
  "$test_root/etc/unionc-agent/config.json"
write_trusted_rpm_backup
sed -i 's/"server_url": "copy-failure-backup"/"server_url": "copy-failure-original"/' \
  "$test_root/etc/unionc-agent/config.json"
if FAIL_RESTORE_COPY=1 "$test_root/postremove.sh" 0 \
  >"$test_root/rpm-restore-copy-failure.log" 2>&1; then
  fail 'RPM postremove accepted a failed restore copy'
fi
grep -F '"server_url": "copy-failure-original"' \
  "$test_root/etc/unionc-agent/config.json" >/dev/null ||
  fail 'failed restore copy changed the original config'
assert_exists "$test_root/var/lib/unionc-agent-package/config.json.remove-backup"
assert_no_restore_temporary

reset_safe_reinstall_state
sed -i 's/"server_url": null/"server_url": "metadata-failure-backup"/' \
  "$test_root/etc/unionc-agent/config.json"
write_trusted_rpm_backup
sed -i 's/"server_url": "metadata-failure-backup"/"server_url": "metadata-failure-original"/' \
  "$test_root/etc/unionc-agent/config.json"
if STAT_CONFIG_RESTORE=0:998:600 "$test_root/postremove.sh" 0 \
  >"$test_root/rpm-restore-temporary-mode.log" 2>&1; then
  fail 'RPM postremove committed a restore temporary with unsafe metadata'
fi
grep -F '"server_url": "metadata-failure-original"' \
  "$test_root/etc/unionc-agent/config.json" >/dev/null ||
  fail 'invalid restore temporary changed the original config'
assert_exists "$test_root/var/lib/unionc-agent-package/config.json.remove-backup"
assert_no_restore_temporary

reset_safe_reinstall_state
write_trusted_rpm_backup
rm -f "$test_root/etc/unionc-agent/config.json"
if STAT_MANAGED_GROUP=0:0:666 "$test_root/postremove.sh" 0 \
  >"$test_root/rpm-restore-open-marker.log" 2>&1; then
  fail 'RPM postremove trusted a writable ownership marker'
fi
assert_exists "$test_root/var/lib/unionc-agent-package/config.json.remove-backup"
assert_absent "$test_root/etc/unionc-agent/config.json"

reset_safe_reinstall_state
write_trusted_rpm_backup
sed -i "s/$package_version/0.3.1/" \
  "$test_root/var/lib/unionc-agent-package/config.json.remove-backup"
rm -f "$test_root/etc/unionc-agent/config.json"
if "$test_root/postremove.sh" 0 >"$test_root/rpm-restore-stale-backup.log" 2>&1; then
  fail 'RPM postremove restored a backup from another Agent version'
fi
assert_exists "$test_root/var/lib/unionc-agent-package/config.json.remove-backup"
assert_absent "$test_root/etc/unionc-agent/config.json"

reset_safe_reinstall_state
sed -i 's/"server_url": null/"server_url": "restore-payload"/' \
  "$test_root/etc/unionc-agent/config.json"
write_trusted_rpm_backup
sed -i 's/"server_url": "restore-payload"/"server_url": "foreign-destination"/' \
  "$test_root/etc/unionc-agent/config.json"
rm -rf -- "$test_root/foreign-rpm-restore-dir"
mv "$test_root/etc/unionc-agent" "$test_root/foreign-rpm-restore-dir"
ln -s "$test_root/foreign-rpm-restore-dir" "$test_root/etc/unionc-agent"
if "$test_root/postremove.sh" 0 >"$test_root/rpm-restore-config-dir-symlink.log" 2>&1; then
  fail 'RPM postremove restored through a symlinked config directory'
fi
grep -F '"server_url": "foreign-destination"' \
  "$test_root/foreign-rpm-restore-dir/config.json" >/dev/null ||
  fail 'RPM restore changed a foreign config directory'
assert_exists "$test_root/var/lib/unionc-agent-package/config.json.remove-backup"

reset_safe_reinstall_state
sed -i 's/"server_url": null/"server_url": "restore-payload"/' \
  "$test_root/etc/unionc-agent/config.json"
write_trusted_rpm_backup
rm -f -- "$test_root/foreign-rpm-restore-file"
mv "$test_root/etc/unionc-agent/config.json" "$test_root/foreign-rpm-restore-file"
sed -i 's/"server_url": "restore-payload"/"server_url": "foreign-file"/' \
  "$test_root/foreign-rpm-restore-file"
ln -s "$test_root/foreign-rpm-restore-file" "$test_root/etc/unionc-agent/config.json"
if "$test_root/postremove.sh" 0 >"$test_root/rpm-restore-config-symlink.log" 2>&1; then
  fail 'RPM postremove restored through a symlinked config file'
fi
grep -F '"server_url": "foreign-file"' "$test_root/foreign-rpm-restore-file" >/dev/null ||
  fail 'RPM restore changed a foreign config file'
assert_exists "$test_root/var/lib/unionc-agent-package/config.json.remove-backup"

reset_safe_reinstall_state
write_trusted_rpm_backup
rm -f "$test_root/etc/unionc-agent/config.json"
if AGENT_GID=997 "$test_root/postremove.sh" 0 \
  >"$test_root/rpm-restore-replaced-group.log" 2>&1; then
  fail 'RPM postremove restored config for a replaced service group'
fi
assert_exists "$test_root/var/lib/unionc-agent-package/config.json.remove-backup"
assert_absent "$test_root/etc/unionc-agent/config.json"

reset_safe_reinstall_state
write_trusted_rpm_backup
write_account_markers 998 998 997
rm -rf "$test_root/etc/unionc-agent"
if "$test_root/postremove.sh" 0 >"$test_root/rpm-restore-replaced-group-marker.log" 2>&1; then
  fail 'RPM postremove restored config for a group not bound by its marker'
fi
assert_exists "$test_root/var/lib/unionc-agent-package/config.json.remove-backup"
assert_absent "$test_root/etc/unionc-agent"

# A failed restore rename leaves both the original config and trusted backup;
# clearing the injected failure makes the exact same transaction retryable.
reset_safe_reinstall_state
sed -i 's/"server_url": null/"server_url": "retry-payload"/' \
  "$test_root/etc/unionc-agent/config.json"
write_trusted_rpm_backup
sed -i 's/"server_url": "retry-payload"/"server_url": "original-config"/' \
  "$test_root/etc/unionc-agent/config.json"
if FAIL_RESTORE_MOVE=1 "$test_root/postremove.sh" 0 \
  >"$test_root/rpm-restore-atomic-move.log" 2>&1; then
  fail 'RPM postremove accepted a failed atomic config restore'
fi
grep -F '"server_url": "original-config"' "$test_root/etc/unionc-agent/config.json" >/dev/null ||
  fail 'failed RPM restore replaced the original config'
assert_exists "$test_root/var/lib/unionc-agent-package/config.json.remove-backup"
assert_no_restore_temporary
"$test_root/postremove.sh" 0 >"$test_root/rpm-restore-retry.log"
grep -F '"server_url": "retry-payload"' "$test_root/etc/unionc-agent/config.json" >/dev/null ||
  fail 'RPM restore retry did not recover the trusted backup'
assert_absent "$test_root/var/lib/unionc-agent-package/config.json.remove-backup"

# Debian remove preserves all local state and does not touch the account.
reset_safe_reinstall_state
: >"$test_root/var/lib/unionc-agent/agent-token"
write_trusted_rpm_backup
sed -i 's/"server_url": null/"server_url": "debian-current"/' \
  "$test_root/etc/unionc-agent/config.json"
: >"$TEST_LOG"
"$test_root/postremove.sh" remove
assert_exists "$test_root/etc/unionc-agent/config.json"
grep -F '"server_url": "debian-current"' "$test_root/etc/unionc-agent/config.json" >/dev/null ||
  fail 'Debian postremove entered the RPM-only config restore path'
assert_exists "$test_root/var/lib/unionc-agent-package/config.json.remove-backup"
assert_exists "$test_root/var/lib/unionc-agent/agent-token"
[ ! -f "$test_root/user.deleted" ] || fail 'remove deleted the service user'

# Debian purge removes fixed local targets and only deletes the account whose
# root-owned ownership markers and expected identity both match.
mkdir -p "$test_root/var/lib/unionc-agent-package"
write_account_markers
: >"$TEST_LOG"
"$test_root/postremove.sh" purge
assert_absent "$test_root/etc/unionc-agent"
assert_absent "$test_root/var/lib/unionc-agent"
assert_absent "$test_root/etc/systemd/system/unionc-agent.service.d"
assert_exists "$test_root/user.deleted"
assert_exists "$test_root/group.deleted"
assert_log_contains 'userdel unionc-agent'
assert_log_contains 'groupdel unionc-agent'
assert_log_contains 'daemon-reload'

# The explicit purge helper must prove both that the unit lookup succeeded and
# that disable --now actually left the service non-running before deleting any
# credential, config, drop-in, or account authorization marker.
prepare_guarded_purge_state() {
  reset_safe_reinstall_state
  rm -rf -- "$test_root/etc/systemd/system/unionc-agent.service.d"
  mkdir -p "$test_root/etc/systemd/system/unionc-agent.service.d"
  : >"$test_root/var/lib/unionc-agent/agent-token"
  : >"$test_root/etc/systemd/system/unionc-agent.service.d/gpu.conf"
}

assert_guarded_purge_state_preserved() {
  assert_exists "$test_root/var/lib/unionc-agent/agent-token"
  assert_exists "$test_root/etc/unionc-agent/config.json"
  assert_exists "$test_root/etc/systemd/system/unionc-agent.service.d/gpu.conf"
  assert_exists "$test_root/var/lib/unionc-agent-package/managed-user"
  assert_exists "$test_root/var/lib/unionc-agent-package/managed-group"
  assert_absent "$test_root/user.deleted"
  assert_absent "$test_root/group.deleted"
}

prepare_guarded_purge_state
: >"$TEST_LOG"
if FAIL_SYSTEMCTL_LOAD_QUERY=1 "$test_root/purge-local-state.sh" --yes \
  >"$test_root/purge-load-query-failed.log" 2>&1; then
  fail 'purge helper treated a failed LoadState query as an absent unit'
fi
assert_guarded_purge_state_preserved
assert_log_contains 'show unionc-agent.service --property=LoadState --value'
if grep -F 'disable --now' "$TEST_LOG" >/dev/null; then
  fail 'purge helper disabled a unit after its LoadState query failed'
fi

prepare_guarded_purge_state
: >"$TEST_LOG"
if EMPTY_SYSTEMCTL_LOAD_QUERY=1 "$test_root/purge-local-state.sh" --yes \
  >"$test_root/purge-load-query-empty.log" 2>&1; then
  fail 'purge helper treated an empty LoadState as an absent unit'
fi
assert_guarded_purge_state_preserved
assert_log_contains 'show unionc-agent.service --property=LoadState --value'
if grep -F 'disable --now' "$TEST_LOG" >/dev/null; then
  fail 'purge helper disabled a unit after receiving an empty LoadState'
fi

prepare_guarded_purge_state
: >"$TEST_LOG"
if SYSTEMCTL_ACTIVE_STATE=active "$test_root/purge-local-state.sh" --yes \
  >"$test_root/purge-service-still-active.log" 2>&1; then
  fail 'purge helper deleted local state while the service remained active'
fi
assert_guarded_purge_state_preserved
assert_log_contains 'disable --now unionc-agent.service'
assert_log_contains 'show unionc-agent.service --property=ActiveState --value'

# The explicit helper refuses accidental invocation without confirmation.
rm -f "$test_root/user.deleted" "$test_root/group.deleted"
mkdir -p "$test_root/var/lib/unionc-agent-package" "$test_root/var/lib/unionc-agent"
write_account_markers
: >"$test_root/var/lib/unionc-agent/agent-token"
if "$test_root/purge-local-state.sh" >"$test_root/purge-no-confirm.log" 2>&1; then
  fail 'purge helper accepted a request without --yes'
fi
assert_exists "$test_root/var/lib/unionc-agent/agent-token"
"$test_root/purge-local-state.sh" --yes
assert_absent "$test_root/var/lib/unionc-agent"
assert_exists "$test_root/user.deleted"
assert_exists "$test_root/group.deleted"

# A missing ownership marker proves nothing by itself. If the matching global
# account still exists (or NSS cannot prove absence), both purge entry points
# must fail before deleting the backup, the other marker, or either account.
reset_safe_reinstall_state
write_trusted_rpm_backup
rm -f "$test_root/var/lib/unionc-agent-package/managed-user" \
  "$test_root/var/lib/unionc-agent-package/managed-group"
if "$test_root/postremove.sh" purge >"$test_root/purge-missing-markers-postremove.log" 2>&1; then
  fail 'postremove purge accepted live accounts without ownership markers'
fi
assert_absent "$test_root/var/lib/unionc-agent"
assert_absent "$test_root/etc/unionc-agent"
assert_exists "$test_root/var/lib/unionc-agent-package/config.json.remove-backup"
assert_absent "$test_root/user.deleted"
assert_absent "$test_root/group.deleted"

reset_safe_reinstall_state
rm -rf "$test_root/var/lib/unionc-agent-package"
if "$test_root/purge-local-state.sh" --yes \
  >"$test_root/purge-missing-bookkeeping-live-accounts.log" 2>&1; then
  fail 'purge helper accepted live accounts after the bookkeeping directory disappeared'
fi
assert_absent "$test_root/user.deleted"
assert_absent "$test_root/group.deleted"

reset_safe_reinstall_state
write_trusted_rpm_backup
rm -f "$test_root/var/lib/unionc-agent-package/managed-user" \
  "$test_root/var/lib/unionc-agent-package/managed-group"
if "$test_root/purge-local-state.sh" --yes \
  >"$test_root/purge-missing-markers-helper.log" 2>&1; then
  fail 'purge helper accepted live accounts without ownership markers'
fi
assert_exists "$test_root/var/lib/unionc-agent-package/config.json.remove-backup"
assert_absent "$test_root/user.deleted"
assert_absent "$test_root/group.deleted"

# Preflight is all-or-nothing: one missing proof cannot let the other valid
# marker delete half of the service identity before the conflict is reported.
reset_safe_reinstall_state
rm -f "$test_root/var/lib/unionc-agent-package/managed-group"
if "$test_root/postremove.sh" purge >"$test_root/purge-missing-group-marker.log" 2>&1; then
  fail 'postremove purge deleted a user before noticing the missing group marker'
fi
assert_exists "$test_root/var/lib/unionc-agent-package/managed-user"
assert_absent "$test_root/user.deleted"
assert_absent "$test_root/group.deleted"

reset_safe_reinstall_state
rm -f "$test_root/var/lib/unionc-agent-package/managed-user"
if "$test_root/purge-local-state.sh" --yes \
  >"$test_root/purge-missing-user-marker.log" 2>&1; then
  fail 'purge helper accepted a live user without its ownership marker'
fi
assert_exists "$test_root/var/lib/unionc-agent-package/managed-group"
assert_absent "$test_root/user.deleted"
assert_absent "$test_root/group.deleted"

reset_safe_reinstall_state
rm -f "$test_root/var/lib/unionc-agent-package/managed-user"
if FAIL_PASSWD_ENUM=1 "$test_root/postremove.sh" purge \
  >"$test_root/purge-missing-user-nss.log" 2>&1; then
  fail 'postremove purge treated an NSS failure as proof that a markerless user was absent'
fi
assert_absent "$test_root/user.deleted"
assert_absent "$test_root/group.deleted"

reset_safe_reinstall_state
rm -f "$test_root/var/lib/unionc-agent-package/managed-group"
if FAIL_GROUP_ENUM=1 "$test_root/purge-local-state.sh" --yes \
  >"$test_root/purge-missing-group-nss.log" 2>&1; then
  fail 'purge helper treated an NSS failure as proof that a markerless group was absent'
fi
assert_absent "$test_root/user.deleted"
assert_absent "$test_root/group.deleted"

# Present markers are also parsed during the all-or-nothing preflight. A valid
# proof for one identity cannot authorize its deletion before a malformed proof
# for the other identity is discovered.
reset_safe_reinstall_state
write_trusted_rpm_backup
printf 'format=0.3.1\ngid=998\n' \
  >"$test_root/var/lib/unionc-agent-package/managed-group"
if "$test_root/postremove.sh" purge >"$test_root/purge-valid-user-bad-group.log" 2>&1; then
  fail 'postremove purge deleted a user before parsing the invalid group marker'
fi
assert_exists "$test_root/var/lib/unionc-agent-package/config.json.remove-backup"
assert_exists "$test_root/var/lib/unionc-agent-package/managed-user"
assert_exists "$test_root/var/lib/unionc-agent-package/managed-group"
assert_absent "$test_root/user.deleted"
assert_absent "$test_root/group.deleted"

reset_safe_reinstall_state
write_trusted_rpm_backup
printf 'format=0.3.1\nuid=998\nprimary_gid=998\n' \
  >"$test_root/var/lib/unionc-agent-package/managed-user"
if "$test_root/purge-local-state.sh" --yes \
  >"$test_root/purge-bad-user-valid-group.log" 2>&1; then
  fail 'purge helper accepted an invalid user marker before processing the group'
fi
assert_exists "$test_root/var/lib/unionc-agent-package/config.json.remove-backup"
assert_exists "$test_root/var/lib/unionc-agent-package/managed-user"
assert_exists "$test_root/var/lib/unionc-agent-package/managed-group"
assert_absent "$test_root/user.deleted"
assert_absent "$test_root/group.deleted"

# Both ownership markers must bind the same live primary group before userdel.
# A syntactically valid group marker with a different GID cannot authorize a
# partial user deletion.
reset_safe_reinstall_state
write_account_markers 998 998 997
if "$test_root/postremove.sh" purge >"$test_root/purge-mismatched-group-binding-postremove.log" 2>&1; then
  fail 'postremove purge accepted ownership markers bound to different groups'
fi
assert_absent "$test_root/user.deleted"
assert_absent "$test_root/group.deleted"
assert_exists "$test_root/var/lib/unionc-agent-package/managed-user"
assert_exists "$test_root/var/lib/unionc-agent-package/managed-group"

reset_safe_reinstall_state
write_account_markers 998 998 997
if "$test_root/purge-local-state.sh" --yes \
  >"$test_root/purge-mismatched-group-binding-helper.log" 2>&1; then
  fail 'purge helper accepted ownership markers bound to different groups'
fi
assert_absent "$test_root/user.deleted"
assert_absent "$test_root/group.deleted"
assert_exists "$test_root/var/lib/unionc-agent-package/managed-user"
assert_exists "$test_root/var/lib/unionc-agent-package/managed-group"

# Marker absence is safe only after the corresponding account is known gone.
# This permits partial cleanup retries and makes a completed purge idempotent.
reset_safe_reinstall_state
rm -f "$test_root/var/lib/unionc-agent-package/managed-user"
: >"$test_root/user.deleted"
"$test_root/postremove.sh" purge >"$test_root/purge-partial-marker-retry.log"
assert_exists "$test_root/group.deleted"
assert_absent "$test_root/var/lib/unionc-agent-package"

reset_safe_reinstall_state
rm -f "$test_root/var/lib/unionc-agent-package/managed-group"
: >"$test_root/group.deleted"
if "$test_root/postremove.sh" purge >"$test_root/purge-group-gone-before-user.log" 2>&1; then
  fail 'postremove purge accepted a live user after its required group disappeared'
fi
assert_absent "$test_root/user.deleted"
assert_exists "$test_root/var/lib/unionc-agent-package/managed-user"

reset_safe_reinstall_state
rm -f "$test_root/var/lib/unionc-agent-package/managed-user" \
  "$test_root/var/lib/unionc-agent-package/managed-group"
START_ACCOUNTS_ABSENT=1 "$test_root/postremove.sh" purge \
  >"$test_root/purge-already-absent.log"
assert_absent "$test_root/var/lib/unionc-agent-package"
START_ACCOUNTS_ABSENT=1 "$test_root/purge-local-state.sh" --yes \
  >"$test_root/purge-idempotent-helper.log"

# Account ownership markers authorize root to delete global OS identities, so
# purge must reject bookkeeping that is not the exact root-only layout created
# by postinstall. Fixed Agent state is still removed, while every file under an
# untrusted bookkeeping root and both accounts are preserved for inspection.
reset_safe_reinstall_state
: >"$test_root/var/lib/unionc-agent-package/config.json.remove-backup"
if STAT_ACCOUNT_STATE=1000:1000:700 "$test_root/postremove.sh" purge \
  >"$test_root/purge-foreign-bookkeeping-owner.log" 2>&1; then
  fail 'postremove purge trusted a non-root account bookkeeping directory'
fi
assert_absent "$test_root/var/lib/unionc-agent"
assert_absent "$test_root/etc/unionc-agent"
assert_exists "$test_root/var/lib/unionc-agent-package/config.json.remove-backup"
assert_exists "$test_root/var/lib/unionc-agent-package/managed-user"
assert_exists "$test_root/var/lib/unionc-agent-package/managed-group"
assert_absent "$test_root/user.deleted"
assert_absent "$test_root/group.deleted"

reset_safe_reinstall_state
: >"$test_root/var/lib/unionc-agent-package/config.json.remove-backup"
if STAT_ACCOUNT_STATE=0:0:777 "$test_root/purge-local-state.sh" --yes \
  >"$test_root/purge-open-bookkeeping-mode.log" 2>&1; then
  fail 'purge helper trusted an account bookkeeping directory with an unsafe mode'
fi
assert_exists "$test_root/var/lib/unionc-agent-package/config.json.remove-backup"
assert_exists "$test_root/var/lib/unionc-agent-package/managed-user"
assert_exists "$test_root/var/lib/unionc-agent-package/managed-group"
assert_absent "$test_root/user.deleted"
assert_absent "$test_root/group.deleted"

# A trusted parent is insufficient when either authorization marker itself is
# writable by another identity. Validate every present marker before deleting
# the backup, either account, or either marker.
reset_safe_reinstall_state
: >"$test_root/var/lib/unionc-agent-package/config.json.remove-backup"
if STAT_MANAGED_USER=1000:1000:600 "$test_root/postremove.sh" purge \
  >"$test_root/purge-foreign-user-marker.log" 2>&1; then
  fail 'postremove purge trusted a non-root managed-user marker'
fi
assert_exists "$test_root/var/lib/unionc-agent-package/config.json.remove-backup"
assert_exists "$test_root/var/lib/unionc-agent-package/managed-user"
assert_exists "$test_root/var/lib/unionc-agent-package/managed-group"
assert_absent "$test_root/user.deleted"
assert_absent "$test_root/group.deleted"

reset_safe_reinstall_state
: >"$test_root/var/lib/unionc-agent-package/config.json.remove-backup"
if STAT_MANAGED_GROUP=0:0:666 "$test_root/purge-local-state.sh" --yes \
  >"$test_root/purge-open-group-marker.log" 2>&1; then
  fail 'purge helper trusted a group marker with an unsafe mode'
fi
assert_exists "$test_root/var/lib/unionc-agent-package/config.json.remove-backup"
assert_exists "$test_root/var/lib/unionc-agent-package/managed-user"
assert_exists "$test_root/var/lib/unionc-agent-package/managed-group"
assert_absent "$test_root/user.deleted"
assert_absent "$test_root/group.deleted"

prepare_symlinked_purge_bookkeeping() {
  reset_safe_reinstall_state
  foreign_bookkeeping="$test_root/foreign-purge-bookkeeping"
  rm -rf -- "$foreign_bookkeeping"
  : >"$test_root/var/lib/unionc-agent-package/config.json.remove-backup"
  mv "$test_root/var/lib/unionc-agent-package" "$foreign_bookkeeping"
  : >"$foreign_bookkeeping/sentinel"
  ln -s "$foreign_bookkeeping" "$test_root/var/lib/unionc-agent-package"
}

assert_foreign_purge_bookkeeping_preserved() {
  assert_exists "$test_root/foreign-purge-bookkeeping/sentinel"
  assert_exists "$test_root/foreign-purge-bookkeeping/config.json.remove-backup"
  assert_exists "$test_root/foreign-purge-bookkeeping/managed-user"
  assert_exists "$test_root/foreign-purge-bookkeeping/managed-group"
  assert_absent "$test_root/user.deleted"
  assert_absent "$test_root/group.deleted"
}

prepare_symlinked_purge_bookkeeping
if "$test_root/postremove.sh" purge >"$test_root/purge-bookkeeping-symlink-postremove.log" 2>&1; then
  fail 'postremove purge followed a symlinked account bookkeeping directory'
fi
assert_foreign_purge_bookkeeping_preserved

prepare_symlinked_purge_bookkeeping
if "$test_root/purge-local-state.sh" --yes \
  >"$test_root/purge-bookkeeping-symlink-helper.log" 2>&1; then
  fail 'purge helper followed a symlinked account bookkeeping directory'
fi
assert_foreign_purge_bookkeeping_preserved

# A creation-time numeric marker prevents a later same-name account from being
# mistaken for the package-created identity.
reset_safe_reinstall_state
mkdir -p "$test_root/var/lib/unionc-agent"
: >"$test_root/var/lib/unionc-agent/agent-token"
write_account_markers 997 998 998
if "$test_root/postremove.sh" purge >"$test_root/replaced-user.log" 2>&1; then
  fail 'purge deleted or accepted a reconstructed same-name user'
fi
assert_absent "$test_root/user.deleted"
assert_absent "$test_root/group.deleted"
assert_exists "$test_root/var/lib/unionc-agent-package/managed-user"

# Supplementary membership is usage: deleting the group would silently remove
# another administrator-managed user's authorization.
rm -f "$test_root/user.deleted" "$test_root/group.deleted"
write_account_markers
if SUPPLEMENTARY_MEMBER=other-user "$test_root/postremove.sh" purge \
  >"$test_root/supplementary-group.log" 2>&1; then
  fail 'purge deleted or accepted a group with supplementary members'
fi
assert_exists "$test_root/user.deleted"
assert_absent "$test_root/group.deleted"
assert_exists "$test_root/var/lib/unionc-agent-package/managed-group"
# Clear the simulated membership and complete the interrupted account cleanup.
"$test_root/postremove.sh" purge >/dev/null
assert_exists "$test_root/group.deleted"

# A primary-GID reference by any other enumerated user is also a hard stop.
rm -f "$test_root/user.deleted" "$test_root/group.deleted"
write_account_markers
if OTHER_PRIMARY_GID=998 "$test_root/purge-local-state.sh" --yes \
  >"$test_root/primary-group.log" 2>&1; then
  fail 'purge helper deleted or accepted a group used as another primary gid'
fi
assert_exists "$test_root/user.deleted"
assert_absent "$test_root/group.deleted"
assert_exists "$test_root/var/lib/unionc-agent-package/managed-group"
"$test_root/purge-local-state.sh" --yes >/dev/null
assert_exists "$test_root/group.deleted"

# Enumeration errors must never be interpreted as account absence.
rm -f "$test_root/user.deleted" "$test_root/group.deleted"
write_account_markers
if FAIL_PASSWD_ENUM=1 "$test_root/postremove.sh" purge \
  >"$test_root/passwd-enumeration.log" 2>&1; then
  fail 'purge accepted an unavailable passwd database'
fi
assert_absent "$test_root/user.deleted"
assert_absent "$test_root/group.deleted"
assert_exists "$test_root/var/lib/unionc-agent-package/managed-user"

# Group database failures likewise preserve both identities and their markers.
rm -f "$test_root/user.deleted" "$test_root/group.deleted"
write_account_markers
if FAIL_GROUP_ENUM=1 "$test_root/purge-local-state.sh" --yes \
  >"$test_root/group-enumeration.log" 2>&1; then
  fail 'purge helper accepted an unavailable group database'
fi
assert_absent "$test_root/user.deleted"
assert_absent "$test_root/group.deleted"
assert_exists "$test_root/var/lib/unionc-agent-package/managed-group"

# Current packages require versioned numeric ownership markers. Malformed or
# previous-version markers fail closed without deleting accounts or rewriting bookkeeping.
rm -f "$test_root/user.deleted" "$test_root/group.deleted"
{
  printf 'format=1\nuid=998\nprimary_gid=998\n'
} >"$test_root/var/lib/unionc-agent-package/managed-user"
{
  printf 'format=1\ngid=998\n'
} >"$test_root/var/lib/unionc-agent-package/managed-group"
if "$test_root/postremove.sh" purge >"$test_root/invalid-marker.log" 2>&1; then
  fail 'purge accepted a previous-version ownership marker'
fi
assert_absent "$test_root/user.deleted"
assert_absent "$test_root/group.deleted"
assert_exists "$test_root/var/lib/unionc-agent-package/managed-user"
assert_exists "$test_root/var/lib/unionc-agent-package/managed-group"

# A fresh install records the numeric identities only after creating them.
rm -f "$test_root/user.created" "$test_root/group.created"
rm -rf "$test_root/var/lib/unionc-agent-package" \
  "$test_root/var/lib/unionc-agent" "$test_root/etc/unionc-agent"
write_package_config
START_ACCOUNTS_ABSENT=1 "$test_root/postinstall.sh" >"$test_root/fresh-install.log"
grep -Fx "format=$package_version" "$test_root/var/lib/unionc-agent-package/managed-user" >/dev/null ||
  fail 'fresh install did not version its user marker'
grep -Fx 'uid=998' "$test_root/var/lib/unionc-agent-package/managed-user" >/dev/null ||
  fail 'fresh install did not record the created uid'
grep -Fx 'primary_gid=998' "$test_root/var/lib/unionc-agent-package/managed-user" >/dev/null ||
  fail 'fresh install did not record the created primary gid'
grep -Fx 'gid=998' "$test_root/var/lib/unionc-agent-package/managed-group" >/dev/null ||
  fail 'fresh install did not record the created group gid'

# A reinstall of exactly the current package accepts the current numeric
# ownership binding and restarts the service without replacing the identity.
START_ACCOUNTS_ABSENT=1 "$test_root/postinstall.sh" >"$test_root/current-reinstall.log"
assert_absent "$test_root/user.deleted"
assert_absent "$test_root/group.deleted"

# RPM can be interrupted after preremove has saved the noreplace config but
# before postremove restores it. The private root:root 0600 backup is itself a
# valid recovery input for the next same-version postinstall.
reset_safe_reinstall_state
sed -i 's/"server_url": null/"server_url": "https:\/\/retained.example"/' \
  "$test_root/etc/unionc-agent/config.json"
: >"$TEST_LOG"
"$test_root/preremove.sh" 0
assert_exists "$test_root/var/lib/unionc-agent-package/config.json.remove-backup"
"$test_root/postinstall.sh" >"$test_root/interrupted-rpm-reinstall.log"
assert_absent "$test_root/var/lib/unionc-agent-package/config.json.remove-backup"
grep -F '"server_url": "https://retained.example"' \
  "$test_root/etc/unionc-agent/config.json" >/dev/null ||
  fail 'postinstall did not consume the interrupted RPM config backup'

# Reinstall applies the same marker metadata contract before consuming a
# backup. A writable marker blocks restore and leaves the recovery input intact.
reset_safe_reinstall_state
sed -i 's/"server_url": null/"server_url": "marker-guarded"/' \
  "$test_root/etc/unionc-agent/config.json"
"$test_root/preremove.sh" 0 >/dev/null
: >"$TEST_LOG"
if STAT_MANAGED_USER=0:0:666 "$test_root/postinstall.sh" \
  >"$test_root/reinstall-open-marker.log" 2>&1; then
  fail 'postinstall consumed an RPM backup authorized by a writable marker'
fi
assert_exists "$test_root/var/lib/unionc-agent-package/config.json.remove-backup"
if grep -F 'restart unionc-agent.service' "$TEST_LOG" >/dev/null; then
  fail 'postinstall started service after rejecting RPM backup ownership metadata'
fi

# Postinstall uses its account-creation rollback trap as the restore temporary
# cleanup boundary too. Copy and metadata failures must preserve both inputs.
reset_safe_reinstall_state
sed -i 's/"server_url": null/"server_url": "reinstall-copy-retained"/' \
  "$test_root/etc/unionc-agent/config.json"
"$test_root/preremove.sh" 0 >/dev/null
write_package_config
if FAIL_RESTORE_COPY=1 "$test_root/postinstall.sh" \
  >"$test_root/reinstall-restore-copy.log" 2>&1; then
  fail 'postinstall accepted a failed RPM restore copy'
fi
grep -F '"server_url": null' "$test_root/etc/unionc-agent/config.json" >/dev/null ||
  fail 'failed postinstall restore copy replaced the extracted config'
assert_exists "$test_root/var/lib/unionc-agent-package/config.json.remove-backup"
assert_no_restore_temporary

reset_safe_reinstall_state
sed -i 's/"server_url": null/"server_url": "reinstall-metadata-retained"/' \
  "$test_root/etc/unionc-agent/config.json"
"$test_root/preremove.sh" 0 >/dev/null
write_package_config
if STAT_CONFIG_RESTORE=0:998:600 "$test_root/postinstall.sh" \
  >"$test_root/reinstall-restore-temporary-mode.log" 2>&1; then
  fail 'postinstall committed an RPM restore temporary with unsafe metadata'
fi
grep -F '"server_url": null' "$test_root/etc/unionc-agent/config.json" >/dev/null ||
  fail 'invalid postinstall restore temporary replaced the extracted config'
assert_exists "$test_root/var/lib/unionc-agent-package/config.json.remove-backup"
assert_no_restore_temporary

# An interrupted reinstall restore is atomic too: a failed rename preserves
# both the newly extracted config and the trusted backup, then a retry succeeds.
reset_safe_reinstall_state
sed -i 's/"server_url": null/"server_url": "reinstall-retained"/' \
  "$test_root/etc/unionc-agent/config.json"
"$test_root/preremove.sh" 0 >/dev/null
write_package_config
if FAIL_RESTORE_MOVE=1 "$test_root/postinstall.sh" \
  >"$test_root/reinstall-restore-move.log" 2>&1; then
  fail 'postinstall accepted a failed atomic RPM config restore'
fi
grep -F '"server_url": null' "$test_root/etc/unionc-agent/config.json" >/dev/null ||
  fail 'failed reinstall restore replaced the extracted package config'
assert_exists "$test_root/var/lib/unionc-agent-package/config.json.remove-backup"
assert_no_restore_temporary
"$test_root/postinstall.sh" >"$test_root/reinstall-restore-retry.log"
grep -F '"server_url": "reinstall-retained"' \
  "$test_root/etc/unionc-agent/config.json" >/dev/null ||
  fail 'postinstall retry did not restore the retained RPM config'
assert_absent "$test_root/var/lib/unionc-agent-package/config.json.remove-backup"

# Marker publication is the account-creation commit point. A failed group
# marker must remove only the exact group created by this invocation and leave
# a clean retry path.
reset_fresh_install_state
if START_ACCOUNTS_ABSENT=1 FAIL_GROUP_MARKER_MOVE=1 "$test_root/postinstall.sh" \
  >"$test_root/group-marker-failure.log" 2>&1; then
  fail 'postinstall accepted a failed managed-group marker publication'
fi
assert_exists "$test_root/group.deleted"
assert_absent "$test_root/user.created"
assert_absent "$test_root/var/lib/unionc-agent-package/managed-group"
assert_absent "$test_root/var/lib/unionc-agent-package/managed-user"
START_ACCOUNTS_ABSENT=1 "$test_root/postinstall.sh" \
  >"$test_root/group-marker-recovery.log"
assert_exists "$test_root/var/lib/unionc-agent-package/managed-group"
assert_exists "$test_root/var/lib/unionc-agent-package/managed-user"

# Once the group marker is committed, a later user-marker failure rolls back
# only the still-uncommitted exact user. The group and its numeric marker stay
# available so a clean rerun can finish the transaction.
reset_fresh_install_state
if START_ACCOUNTS_ABSENT=1 FAIL_USER_MARKER_MOVE=1 "$test_root/postinstall.sh" \
  >"$test_root/user-marker-failure.log" 2>&1; then
  fail 'postinstall accepted a failed managed-user marker publication'
fi
assert_exists "$test_root/user.deleted"
assert_absent "$test_root/group.deleted"
assert_exists "$test_root/var/lib/unionc-agent-package/managed-group"
assert_absent "$test_root/var/lib/unionc-agent-package/managed-user"
START_ACCOUNTS_ABSENT=1 "$test_root/postinstall.sh" \
  >"$test_root/user-marker-recovery.log"
assert_exists "$test_root/var/lib/unionc-agent-package/managed-group"
assert_exists "$test_root/var/lib/unionc-agent-package/managed-user"

# Even a record created moments ago is no longer safe to delete if its numeric
# identity changes before rollback. Preserve the replacement and fail closed.
reset_fresh_install_state
if START_ACCOUNTS_ABSENT=1 FAIL_GROUP_MARKER_MOVE=1 \
  REPLACE_GROUP_BEFORE_ROLLBACK=1 "$test_root/postinstall.sh" \
  >"$test_root/replaced-group-during-rollback.log" 2>&1; then
  fail 'postinstall accepted a replaced group during marker rollback'
fi
assert_absent "$test_root/group.deleted"

reset_fresh_install_state
if START_ACCOUNTS_ABSENT=1 FAIL_USER_MARKER_MOVE=1 \
  REPLACE_USER_BEFORE_ROLLBACK=1 "$test_root/postinstall.sh" \
  >"$test_root/replaced-user-during-rollback.log" 2>&1; then
  fail 'postinstall accepted a replaced user during marker rollback'
fi
assert_absent "$test_root/user.deleted"

# Root-run postinstall must never normalize or traverse a foreign bookkeeping,
# state, or config root. Each rejection happens before service startup.
reset_safe_reinstall_state
if STAT_ACCOUNT_STATE=1000:1000:755 "$test_root/postinstall.sh" \
  >"$test_root/foreign-account-state.log" 2>&1; then
  fail 'postinstall adopted a foreign package account-state directory'
fi

reset_safe_reinstall_state
mkdir -p "$test_root/foreign-account-state"
: >"$test_root/foreign-account-state/sentinel"
rm -rf "$test_root/var/lib/unionc-agent-package"
ln -s "$test_root/foreign-account-state" "$test_root/var/lib/unionc-agent-package"
if "$test_root/postinstall.sh" >"$test_root/symlink-account-state.log" 2>&1; then
  fail 'postinstall followed a symlinked package account-state directory'
fi
assert_exists "$test_root/foreign-account-state/sentinel"

reset_safe_reinstall_state
rm -rf "$test_root/var/lib/unionc-agent-package"
if "$test_root/postinstall.sh" >"$test_root/stale-agent-state.log" 2>&1; then
  fail 'postinstall adopted retained Agent state without ownership markers'
fi
assert_absent "$test_root/var/lib/unionc-agent-package/managed-user"
assert_absent "$test_root/var/lib/unionc-agent-package/managed-group"

reset_safe_reinstall_state
mkdir -p "$test_root/foreign-agent-state"
: >"$test_root/foreign-agent-state/sentinel"
rm -rf "$test_root/var/lib/unionc-agent"
ln -s "$test_root/foreign-agent-state" "$test_root/var/lib/unionc-agent"
if "$test_root/postinstall.sh" >"$test_root/symlink-agent-state.log" 2>&1; then
  fail 'postinstall followed a symlinked Agent state directory'
fi
assert_exists "$test_root/foreign-agent-state/sentinel"

reset_safe_reinstall_state
if STAT_AGENT_STATE=0:0:700 "$test_root/postinstall.sh" \
  >"$test_root/foreign-agent-owner.log" 2>&1; then
  fail 'postinstall adopted Agent state owned by a foreign identity'
fi

reset_safe_reinstall_state
mv "$test_root/etc/unionc-agent" "$test_root/foreign-config-dir"
ln -s "$test_root/foreign-config-dir" "$test_root/etc/unionc-agent"
if "$test_root/postinstall.sh" >"$test_root/symlink-config-dir.log" 2>&1; then
  fail 'postinstall followed a symlinked config directory'
fi

reset_safe_reinstall_state
mv "$test_root/etc/unionc-agent/config.json" "$test_root/foreign-config.json"
ln -s "$test_root/foreign-config.json" "$test_root/etc/unionc-agent/config.json"
if "$test_root/postinstall.sh" >"$test_root/symlink-config.log" 2>&1; then
  fail 'postinstall followed a symlinked config file'
fi

reset_safe_reinstall_state
sed -i "s/$package_version/0.3.1/" "$test_root/etc/unionc-agent/config.json"
if "$test_root/postinstall.sh" >"$test_root/stale-config.log" 2>&1; then
  fail 'postinstall accepted a config from another Agent version'
fi

reset_safe_reinstall_state
if STAT_CONFIG_DIR=0:0:777 "$test_root/postinstall.sh" \
  >"$test_root/foreign-config-mode.log" 2>&1; then
  fail 'postinstall normalized a world-writable config directory'
fi

reset_safe_reinstall_state
rm -f "$test_root/user.created" "$test_root/group.created"
if START_ACCOUNTS_ABSENT=1 "$test_root/postinstall.sh" \
  >"$test_root/stale-account-marker.log" 2>&1; then
  fail 'postinstall recreated a package-managed identity that disappeared'
fi
assert_absent "$test_root/user.created"
assert_absent "$test_root/group.created"

# A live-system install cannot report success when service startup fails.
reset_safe_reinstall_state
: >"$TEST_LOG"
if FAIL_RESTART=1 "$test_root/postinstall.sh" >"$test_root/postinstall-failure.log" 2>&1; then
  fail 'postinstall ignored a service restart failure'
fi
assert_log_contains 'restart unionc-agent.service'
if grep -F 'UnionC Agent 服务已启动' "$test_root/postinstall-failure.log" >/dev/null; then
  fail 'postinstall printed a false success message'
fi
assert_exists "$test_root/var/lib/unionc-agent-package/managed-user"
assert_exists "$test_root/var/lib/unionc-agent-package/managed-group"

# `is-active` is a second guard after the notify-aware restart job completes.
reset_safe_reinstall_state
: >"$TEST_LOG"
if FAIL_ACTIVE=1 "$test_root/postinstall.sh" >"$test_root/postinstall-inactive.log" 2>&1; then
  fail 'postinstall ignored a service that did not remain active'
fi
assert_log_contains 'restart unionc-agent.service'
assert_log_contains 'is-active --quiet unionc-agent.service'
if grep -F 'UnionC Agent 服务已启动' "$test_root/postinstall-inactive.log" >/dev/null; then
  fail 'postinstall printed success for an inactive service'
fi

echo 'Linux packaging lifecycle tests passed'
