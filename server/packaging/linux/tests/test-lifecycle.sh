#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
packaging_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)
test_root=$(mktemp -d "${TMPDIR:-/tmp}/unionc-server-packaging-test.XXXXXX")
package_version=0.3.4
workspace_version=$(sed -n 's/^version = "\([0-9][0-9.]*\)"$/\1/p' \
  "$packaging_dir/../../../Cargo.toml")
[ "$workspace_version" = "$package_version" ] || {
  echo "Server Linux lifecycle version must follow the current Cargo package version" >&2
  exit 1
}

cleanup() {
  case "$test_root" in
    "${TMPDIR:-/tmp}"/unionc-server-packaging-test.*)
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

source_postinstall="$packaging_dir/postinstall.sh"
sh -n "$source_postinstall"
grep -Fx 'PATH=/usr/sbin:/usr/bin:/sbin:/bin' "$source_postinstall" >/dev/null ||
  fail 'root postinstall does not replace the caller PATH'
grep -Fx 'server_binary=/usr/bin/unionc' "$source_postinstall" >/dev/null ||
  fail 'postinstall does not bind version validation to the packaged Server binary'
grep -Fx 'Type=notify' "$packaging_dir/unionc.service" >/dev/null ||
  fail 'systemd unit does not wait for Server readiness'
grep -Fx 'NotifyAccess=main' "$packaging_dir/unionc.service" >/dev/null ||
  fail 'systemd unit accepts readiness from an unexpected process'
grep -Fx 'TimeoutStartSec=60s' "$packaging_dir/unionc.service" >/dev/null ||
  fail 'systemd unit has no bounded readiness timeout'
if grep -Fx 'Type=simple' "$packaging_dir/unionc.service" >/dev/null; then
  fail 'systemd unit can report startup before Server initialization'
fi

# Relocate every package-owned path into a disposable tree. The rewritten PATH
# keeps the test doubles authoritative while preserving the production script's
# rule that a caller-controlled PATH is never trusted.
sed \
  -e 's#^account_database_dir=/etc$#account_database_dir=__ACCOUNT_DATABASE_DIR__#' \
  -e 's#/var/lib/unionc-package#__PACKAGE_STATE__#g' \
  -e 's#/var/lib/unionc#__SERVER_STATE__#g' \
  -e 's#/etc/login.defs#__LOGIN_DEFS__#g' \
  -e 's#/etc/unionc/unionc.env#__PACKAGE_CONFIG__#g' \
  -e 's#/etc/unionc#__CONFIG_DIR__#g' \
  -e 's#/run/systemd/system#__SYSTEMD_RUNTIME__#g' \
  -e 's#/usr/bin/unionc#__SERVER_BINARY__#g' \
  -e 's#^PATH=/usr/sbin:/usr/bin:/sbin:/bin$#PATH=__TRUSTED_BIN__:/usr/sbin:/usr/bin:/sbin:/bin#' \
  -e "s#__PACKAGE_STATE__#$test_root/var/lib/unionc-package#g" \
  -e "s#__SERVER_STATE__#$test_root/var/lib/unionc#g" \
  -e "s#__LOGIN_DEFS__#$test_root/etc/login.defs#g" \
  -e "s#__ACCOUNT_DATABASE_DIR__#$test_root/etc#g" \
  -e "s#__PACKAGE_CONFIG__#$test_root/etc/unionc/unionc.env#g" \
  -e "s#__CONFIG_DIR__#$test_root/etc/unionc#g" \
  -e "s#__SYSTEMD_RUNTIME__#$test_root/run/systemd/system#g" \
  -e "s#__SERVER_BINARY__#$test_root/trusted-bin/unionc#g" \
  -e "s#__TRUSTED_BIN__#$test_root/trusted-bin#g" \
  "$source_postinstall" >"$test_root/postinstall.sh"
chmod 0755 "$test_root/postinstall.sh"

mkdir -p \
  "$test_root/trusted-bin" \
  "$test_root/attacker-bin" \
  "$test_root/etc/unionc" \
  "$test_root/var/lib/unionc-package" \
  "$test_root/var/lib/unionc"
export test_root

write_test_login_defs() {
  cat >"$test_root/etc/login.defs" <<'EOF'
UID_MIN 1000
GID_MIN 1000
SYS_UID_MIN 900
SYS_UID_MAX 999
SYS_GID_MIN 900
SYS_GID_MAX 999
EOF
}
write_test_login_defs

cat >"$test_root/trusted-bin/unionc" <<EOF
#!/bin/sh
: >"$test_root/trusted-server-ran"
printf 'unionc %s\n' '$package_version'
EOF

cat >"$test_root/trusted-bin/getent" <<'EOF'
#!/bin/sh
current_group_gid=998
current_user_uid=998
current_user_gid=998
if [ -f "$test_root/current-group-gid" ]; then
  IFS= read -r current_group_gid <"$test_root/current-group-gid"
fi
if [ -f "$test_root/current-user-uid" ]; then
  IFS= read -r current_user_uid <"$test_root/current-user-uid"
fi
if [ -f "$test_root/current-user-gid" ]; then
  IFS= read -r current_user_gid <"$test_root/current-user-gid"
fi
group_exists() {
  if [ "${START_ACCOUNTS_ABSENT:-0}" -eq 1 ]; then
    [ -f "$test_root/group.created" ] && [ ! -f "$test_root/group.deleted" ]
  else
    [ ! -f "$test_root/group.deleted" ]
  fi
}
user_exists() {
  if [ "${START_ACCOUNTS_ABSENT:-0}" -eq 1 ]; then
    [ -f "$test_root/user.created" ] && [ ! -f "$test_root/user.deleted" ]
  else
    [ ! -f "$test_root/user.deleted" ]
  fi
}
case "${1:-}:${2:-}" in
  group:unionc)
    group_exists || exit 2
    printf 'unionc:x:%s:\n' "$current_group_gid"
    ;;
  group:)
    [ "${FAIL_GROUP_ENUM:-0}" -ne 1 ] || exit 2
    if [ "${FAIL_GROUP_ENUM_ONCE_AFTER_CREATE:-0}" -eq 1 ] &&
      [ -f "$test_root/group.created" ] &&
      [ ! -f "$test_root/group-enum-failed-once" ]; then
      : >"$test_root/group-enum-failed-once"
      exit 2
    fi
    if group_exists; then
      printf 'unionc:x:%s:\n' "$current_group_gid"
    fi
    if [ -f "$test_root/extra-group-entries" ]; then
      /usr/bin/cat "$test_root/extra-group-entries"
    fi
    exit 0
    ;;
  passwd:unionc)
    user_exists || exit 2
    printf 'unionc:x:%s:%s::%s/var/lib/unionc:/usr/sbin/nologin\n' \
      "$current_user_uid" "$current_user_gid" "$test_root"
    ;;
  passwd:)
    [ "${FAIL_PASSWD_ENUM:-0}" -ne 1 ] || exit 2
    if [ "${FAIL_PASSWD_ENUM_ONCE_AFTER_CREATE:-0}" -eq 1 ] &&
      [ -f "$test_root/user.created" ] &&
      [ ! -f "$test_root/passwd-enum-failed-once" ]; then
      : >"$test_root/passwd-enum-failed-once"
      exit 2
    fi
    if user_exists; then
      printf 'unionc:x:%s:%s::%s/var/lib/unionc:/usr/sbin/nologin\n' \
        "$current_user_uid" "$current_user_gid" "$test_root"
    fi
    if [ -f "$test_root/extra-passwd-entries" ]; then
      /usr/bin/cat "$test_root/extra-passwd-entries"
    fi
    exit 0
    ;;
  gshadow:)
    [ "${FAIL_GSHADOW_ENUM:-0}" -ne 1 ] || exit 2
    if group_exists && [ ! -f "$test_root/group-shadow-missing" ]; then
      printf 'unionc:!::\n'
    fi
    exit 0
    ;;
  shadow:)
    [ "${FAIL_SHADOW_ENUM:-0}" -ne 1 ] || exit 2
    if user_exists && [ ! -f "$test_root/user-shadow-missing" ]; then
      printf 'unionc:!:20000::::::\n'
    fi
    exit 0
    ;;
  *) exit 2 ;;
esac
EOF

cat >"$test_root/trusted-bin/groupadd" <<'EOF'
#!/bin/sh
[ "${START_ACCOUNTS_ABSENT:-0}" -eq 1 ]
selected_gid=
while [ "$#" -gt 0 ]; do
  if [ "$1" = --gid ]; then
    shift
    selected_gid=${1:-}
  fi
  shift
done
case "$selected_gid" in
  ''|*[!0-9]*) exit 70 ;;
esac
[ "${OCCUPY_GROUP_GID_BEFORE_CREATE:-0}" -ne 1 ] || {
  printf 'other:x:%s:\n' "$selected_gid" >"$test_root/extra-group-entries"
  printf 'groupadd gid=%s\n' "$selected_gid" >>"$TEST_LOG"
  exit 77
}
[ "${FAIL_GROUPADD_WITHOUT_CREATE:-0}" -ne 1 ] || {
  printf 'groupadd gid=%s\n' "$selected_gid" >>"$TEST_LOG"
  exit 71
}
rm -f "$test_root/group.deleted"
: >"$test_root/group.created"
printf '%s\n' "$selected_gid" >"$test_root/current-group-gid"
printf 'groupadd gid=%s\n' "$selected_gid" >>"$TEST_LOG"
[ "${GROUPADD_PUBLIC_ONLY:-0}" -ne 1 ] || {
  : >"$test_root/group-shadow-missing"
  exit 73
}
[ "${FAIL_GROUPADD_AFTER_CREATE:-0}" -ne 1 ] || exit 72
EOF

cat >"$test_root/trusted-bin/useradd" <<'EOF'
#!/bin/sh
[ "${START_ACCOUNTS_ABSENT:-0}" -eq 1 ]
selected_uid=
while [ "$#" -gt 0 ]; do
  if [ "$1" = --uid ]; then
    shift
    selected_uid=${1:-}
  fi
  shift
done
case "$selected_uid" in
  ''|*[!0-9]*) exit 74 ;;
esac
[ "${OCCUPY_USER_UID_BEFORE_CREATE:-0}" -ne 1 ] || {
  printf 'other:x:%s:123::/nonexistent:/usr/sbin/nologin\n' "$selected_uid" \
    >"$test_root/extra-passwd-entries"
  printf 'useradd uid=%s\n' "$selected_uid" >>"$TEST_LOG"
  exit 77
}
[ "${FAIL_USERADD_WITHOUT_CREATE:-0}" -ne 1 ] || {
  printf 'useradd uid=%s\n' "$selected_uid" >>"$TEST_LOG"
  exit 75
}
rm -f "$test_root/user.deleted"
: >"$test_root/user.created"
printf '%s\n' "$selected_uid" >"$test_root/current-user-uid"
if [ -f "$test_root/current-group-gid" ]; then
  /usr/bin/cp "$test_root/current-group-gid" "$test_root/current-user-gid"
fi
printf 'useradd uid=%s\n' "$selected_uid" >>"$TEST_LOG"
[ "${USERADD_PUBLIC_ONLY:-0}" -ne 1 ] || {
  : >"$test_root/user-shadow-missing"
  exit 73
}
[ "${FAIL_USERADD_AFTER_CREATE:-0}" -ne 1 ] || exit 76
EOF

cat >"$test_root/trusted-bin/groupdel" <<'EOF'
#!/bin/sh
[ "${1:-}" = unionc ]
: >"$test_root/group.deleted"
printf 'groupdel %s\n' "$*" >>"$TEST_LOG"
EOF

cat >"$test_root/trusted-bin/userdel" <<'EOF'
#!/bin/sh
[ "${1:-}" = unionc ]
: >"$test_root/user.deleted"
printf 'userdel %s\n' "$*" >>"$TEST_LOG"
EOF

cat >"$test_root/trusted-bin/install" <<'EOF'
#!/bin/sh
printf 'install %s\n' "$*" >>"$TEST_LOG"
destination=
for argument in "$@"; do
  destination=$argument
done
[ -n "$destination" ]
/usr/bin/mkdir -p -- "$destination"
EOF

for command_name in chown chmod; do
  cat >"$test_root/trusted-bin/$command_name" <<'EOF'
#!/bin/sh
printf '%s %s\n' "${0##*/}" "$*" >>"$TEST_LOG"
if [ "${MODEL_CONFIG_CHOWN:-0}" -eq 1 ] && [ "${0##*/}" = chown ]; then
  owner=${1:-}
  destination=
  for argument in "$@"; do
    destination=$argument
  done
  if [ "$destination" = "$test_root/etc/unionc/unionc.env" ]; then
    case "$owner" in
      root:*)
        config_gid=${owner#root:}
        case "$config_gid" in
          ''|*[!0-9]*) exit 2 ;;
        esac
        printf '%s\n' "$config_gid" >"$test_root/config-chowned-gid"
        ;;
    esac
  fi
fi
exit 0
EOF
done

cat >"$test_root/trusted-bin/stat" <<'EOF'
#!/bin/sh
format=
metadata_path=
while [ "$#" -gt 0 ]; do
  case "$1" in
    -c)
      shift
      format=$1
      ;;
    --) ;;
    *) metadata_path=$1 ;;
  esac
  shift
done
case "$metadata_path" in
  "$test_root/var/lib/unionc-package")
    metadata=${STAT_ACCOUNT_STATE:-0:0:700}
    nlink=${STAT_ACCOUNT_STATE_NLINK:-2}
    ;;
  "$test_root/var/lib/unionc-package/managed-user")
    metadata=${STAT_MANAGED_USER:-0:0:600}
    nlink=${STAT_MANAGED_USER_NLINK:-1}
    ;;
  "$test_root/var/lib/unionc-package/managed-group")
    metadata=${STAT_MANAGED_GROUP:-0:0:600}
    nlink=${STAT_MANAGED_GROUP_NLINK:-1}
    ;;
  "$test_root/var/lib/unionc-package/pending-user")
    metadata=${STAT_PENDING_USER:-0:0:600}
    nlink=${STAT_PENDING_USER_NLINK:-1}
    ;;
  "$test_root/var/lib/unionc-package/pending-group")
    metadata=${STAT_PENDING_GROUP:-0:0:600}
    nlink=${STAT_PENDING_GROUP_NLINK:-1}
    ;;
  "$test_root/var/lib/unionc-package/".managed-*|\
  "$test_root/var/lib/unionc-package/".pending-*)
    metadata=0:0:600
    nlink=1
    ;;
  "$test_root/var/lib/unionc")
    data_uid=998
    data_gid=998
    if [ -f "$test_root/current-user-uid" ]; then
      IFS= read -r data_uid <"$test_root/current-user-uid"
    fi
    if [ -f "$test_root/current-user-gid" ]; then
      IFS= read -r data_gid <"$test_root/current-user-gid"
    fi
    metadata="$data_uid:$data_gid:700"
    nlink=2
    ;;
  "$test_root/etc/unionc")
    metadata=${STAT_CONFIG_DIR:-0:0:755}
    nlink=${STAT_CONFIG_DIR_NLINK:-2}
    ;;
  "$test_root/etc")
    metadata=${STAT_ACCOUNT_DATABASE_DIR:-0:0:755}
    nlink=${STAT_ACCOUNT_DATABASE_DIR_NLINK:-2}
    ;;
  "$test_root/etc/unionc/unionc.env")
    if [ "${MODEL_CONFIG_CHOWN:-0}" -eq 1 ] &&
      [ -f "$test_root/config-chowned-gid" ]; then
      IFS= read -r config_gid <"$test_root/config-chowned-gid"
      metadata="0:$config_gid:640"
    else
      metadata=${STAT_CONFIG_FILE:-0:998:640}
    fi
    nlink=${STAT_CONFIG_FILE_NLINK:-1}
    ;;
  "$test_root/etc/login.defs")
    metadata=${STAT_LOGIN_DEFS:-0:0:644}
    nlink=${STAT_LOGIN_DEFS_NLINK:-1}
    ;;
  *) exec /usr/bin/stat -c "$format" -- "$metadata_path" ;;
esac
uid=${metadata%%:*}
remainder=${metadata#*:}
gid=${remainder%%:*}
mode=${metadata##*:}
case "$format" in
  %u) printf '%s\n' "$uid" ;;
  %g) printf '%s\n' "$gid" ;;
  %a) printf '%s\n' "$mode" ;;
  %s) exec /usr/bin/stat -c %s -- "$metadata_path" ;;
  %u:%g:%a) printf '%s:%s:%s\n' "$uid" "$gid" "$mode" ;;
  %u:%g:%a:%h) printf '%s:%s:%s:%s\n' "$uid" "$gid" "$mode" "$nlink" ;;
  *) exit 2 ;;
esac
EOF

cat >"$test_root/trusted-bin/mv" <<'EOF'
#!/bin/sh
destination=
for argument in "$@"; do
  destination=$argument
done
printf 'mv %s\n' "$destination" >>"$TEST_LOG"
case "$destination" in
  "$test_root/var/lib/unionc-package/managed-group")
    if [ "${FAIL_GROUP_MARKER_MOVE_AFTER_RENAME:-0}" -eq 1 ]; then
      /usr/bin/mv "$@"
      exit 75
    fi
    if [ "${FAIL_GROUP_MARKER_MOVE:-0}" -eq 1 ]; then
      if [ "${REPLACE_GROUP_BEFORE_ROLLBACK:-0}" -eq 1 ]; then
        printf '997\n' >"$test_root/current-group-gid"
      fi
      exit 73
    fi
    ;;
  "$test_root/var/lib/unionc-package/managed-user")
    if [ "${FAIL_USER_MARKER_MOVE_AFTER_RENAME:-0}" -eq 1 ]; then
      /usr/bin/mv "$@"
      exit 76
    fi
    if [ "${FAIL_USER_MARKER_MOVE:-0}" -eq 1 ]; then
      if [ "${REPLACE_USER_BEFORE_ROLLBACK:-0}" -eq 1 ]; then
        printf '997\n' >"$test_root/current-user-uid"
      fi
      exit 74
    fi
    ;;
esac
exec /usr/bin/mv "$@"
EOF

cat >"$test_root/trusted-bin/sync" <<'EOF'
#!/bin/sh
sync_target=
for argument in "$@"; do
  sync_target=$argument
done
printf 'sync %s\n' "$sync_target" >>"$TEST_LOG"
if [ "$sync_target" = "$test_root/etc" ]; then
  if [ "${FAIL_GROUP_ACCOUNT_SYNC_ONCE:-0}" -eq 1 ] &&
    [ -f "$test_root/group.created" ] &&
    [ -f "$test_root/var/lib/unionc-package/pending-group" ] &&
    [ ! -f "$test_root/var/lib/unionc-package/managed-group" ] &&
    [ ! -f "$test_root/group-account-sync-failed-once" ]; then
    : >"$test_root/group-account-sync-failed-once"
    exit 78
  fi
  if [ "${FAIL_USER_ACCOUNT_SYNC_ONCE:-0}" -eq 1 ] &&
    [ -f "$test_root/user.created" ] &&
    [ -f "$test_root/var/lib/unionc-package/pending-user" ] &&
    [ ! -f "$test_root/var/lib/unionc-package/managed-user" ] &&
    [ ! -f "$test_root/user-account-sync-failed-once" ]; then
    : >"$test_root/user-account-sync-failed-once"
    exit 79
  fi
fi
if [ "${FAIL_PENDING_GROUP_TEMP_SYNC:-0}" -eq 1 ] &&
  [ "$sync_target" = "$test_root/var/lib/unionc-package/.pending-group.new" ]; then
  exit 80
fi
if [ "$sync_target" = "$test_root/var/lib/unionc-package" ]; then
  if [ "${FAIL_PENDING_GROUP_DIR_SYNC:-0}" -eq 1 ] &&
    [ -f "$test_root/var/lib/unionc-package/pending-group" ] &&
    [ ! -f "$test_root/group.created" ] &&
    [ ! -f "$test_root/var/lib/unionc-package/managed-group" ]; then
    exit 81
  fi
  if [ "${FAIL_MANAGED_GROUP_DIR_SYNC_ONCE:-0}" -eq 1 ] &&
    [ -f "$test_root/var/lib/unionc-package/pending-group" ] &&
    [ -f "$test_root/var/lib/unionc-package/managed-group" ] &&
    [ ! -f "$test_root/managed-group-sync-failed-once" ]; then
    : >"$test_root/managed-group-sync-failed-once"
    exit 82
  fi
  if [ "${FAIL_GROUP_PENDING_CLEANUP_SYNC_ONCE:-0}" -eq 1 ] &&
    [ ! -e "$test_root/var/lib/unionc-package/pending-group" ] &&
    [ -f "$test_root/var/lib/unionc-package/managed-group" ] &&
    [ ! -f "$test_root/group-pending-cleanup-sync-failed-once" ] &&
    [ ! -f "$test_root/user.created" ]; then
    : >"$test_root/group-pending-cleanup-sync-failed-once"
    exit 83
  fi
fi
exit 0
EOF

cat >"$test_root/trusted-bin/systemctl" <<'EOF'
#!/bin/sh
printf 'systemctl %s\n' "$*" >>"$TEST_LOG"
case "${1:-}" in
  is-enabled)
    [ "${SERVICE_ENABLED:-0}" -eq 1 ]
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

for attacker_command in unionc getent groupadd groupdel useradd userdel install cut chown chmod rm mv stat sync awk systemctl; do
  cat >"$test_root/attacker-bin/$attacker_command" <<'EOF'
#!/bin/sh
: >"$test_root/attacker-command-ran"
exit 99
EOF
done

chmod 0755 "$test_root/trusted-bin/"* "$test_root/attacker-bin/"*
/usr/bin/sync -f "$test_root"
TEST_LOG="$test_root/commands.log"
export TEST_LOG
: >"$TEST_LOG"

cat >"$test_root/etc/unionc/unionc.env" <<EOF
UNIONC_PACKAGE_VERSION=$package_version
EOF
cat >"$test_root/var/lib/unionc-package/managed-user" <<EOF
format=$package_version
uid=998
primary_gid=998
EOF
cat >"$test_root/var/lib/unionc-package/managed-group" <<EOF
format=$package_version
gid=998
EOF

# Package managers execute lifecycle hooks as root, but their inherited PATH is
# not an authority boundary. No executable supplied by the caller may run.
PATH="$test_root/attacker-bin" "$test_root/postinstall.sh" \
  >"$test_root/postinstall.log" 2>&1 ||
  fail 'postinstall failed after replacing an untrusted caller PATH'
[ -e "$test_root/trusted-server-ran" ] ||
  fail 'postinstall did not execute the packaged Server binary'
[ ! -e "$test_root/attacker-command-ran" ] ||
  fail 'postinstall executed a command from the caller PATH'
grep -Fx "chown root:998 $test_root/etc/unionc/unionc.env" "$TEST_LOG" >/dev/null ||
  fail 'postinstall did not bind config ownership to the recorded numeric group'
grep -Fx "chmod 0640 $test_root/etc/unionc/unionc.env" "$TEST_LOG" >/dev/null ||
  fail 'postinstall did not secure the Server config mode'

# An enabled reinstall must fail unless the notify-aware startup job reaches
# readiness and the resulting service remains active.
mkdir -p "$test_root/run/systemd/system"
: >"$TEST_LOG"
if SERVICE_ENABLED=1 FAIL_RESTART=1 "$test_root/postinstall.sh" \
  >"$test_root/restart-failure.log" 2>&1; then
  fail 'postinstall ignored an enabled service readiness failure'
fi
grep -Fx 'systemctl restart unionc.service' "$TEST_LOG" >/dev/null ||
  fail 'postinstall did not restart the enabled service'
grep -F 'did not reach readiness' "$test_root/restart-failure.log" >/dev/null ||
  fail 'postinstall did not diagnose the readiness failure'

: >"$TEST_LOG"
if SERVICE_ENABLED=1 FAIL_ACTIVE=1 "$test_root/postinstall.sh" \
  >"$test_root/inactive-service.log" 2>&1; then
  fail 'postinstall ignored a service that did not remain active'
fi
grep -Fx 'systemctl is-active --quiet unionc.service' "$TEST_LOG" >/dev/null ||
  fail 'postinstall did not verify the restarted service state'
grep -F 'did not remain active' "$test_root/inactive-service.log" >/dev/null ||
  fail 'postinstall did not diagnose the inactive service'
rmdir "$test_root/run/systemd/system" "$test_root/run/systemd"

# A root hook must reject foreign ownership proof before changing its metadata.
# Otherwise install -d could turn attacker-controlled marker storage into a
# root-owned directory and make its contents appear authoritative.
: >"$TEST_LOG"
if STAT_ACCOUNT_STATE=1000:1000:777 "$test_root/postinstall.sh" \
  >"$test_root/foreign-account-state.log" 2>&1; then
  fail 'postinstall adopted a foreign package account-state directory'
fi
[ ! -s "$TEST_LOG" ] ||
  fail 'postinstall normalized a foreign package account-state directory'

if STAT_MANAGED_USER=1000:1000:600 "$test_root/postinstall.sh" \
  >"$test_root/foreign-user-marker.log" 2>&1; then
  fail 'postinstall trusted a managed-user marker not owned by root'
fi
if STAT_MANAGED_GROUP=0:0:666 "$test_root/postinstall.sh" \
  >"$test_root/writable-group-marker.log" 2>&1; then
  fail 'postinstall trusted a writable managed-group marker'
fi
if STAT_MANAGED_USER_NLINK=2 "$test_root/postinstall.sh" \
  >"$test_root/hardlinked-user-marker.log" 2>&1; then
  fail 'postinstall trusted a hard-linked managed-user marker'
fi

cat >"$test_root/var/lib/unionc-package/pending-group" <<EOF
format=$package_version
state=pending
kind=group
name=unionc
gid=998
EOF
if STAT_PENDING_GROUP=1000:1000:600 "$test_root/postinstall.sh" \
  >"$test_root/foreign-pending-group.log" 2>&1; then
  fail 'postinstall trusted a pending-group marker not owned by root'
fi
if STAT_PENDING_GROUP_NLINK=2 "$test_root/postinstall.sh" \
  >"$test_root/hardlinked-pending-group.log" 2>&1; then
  fail 'postinstall trusted a hard-linked pending-group marker'
fi
awk 'BEGIN { for (i = 0; i < 600; i += 1) printf "x"; printf "\n" }' \
  >>"$test_root/var/lib/unionc-package/pending-group"
if "$test_root/postinstall.sh" >"$test_root/oversized-pending-group.log" 2>&1; then
  fail 'postinstall trusted an oversized pending-group marker'
fi
rm -f "$test_root/var/lib/unionc-package/pending-group"

# The final file can be regular even when an attacker redirected its parent.
# Validate both components and all metadata before the root chown/chmod step.
mv "$test_root/etc/unionc" "$test_root/foreign-config"
ln -s "$test_root/foreign-config" "$test_root/etc/unionc"
if "$test_root/postinstall.sh" >"$test_root/symlink-config-dir.log" 2>&1; then
  fail 'postinstall followed a symlinked Server config directory'
fi
rm -f "$test_root/etc/unionc"
mv "$test_root/foreign-config" "$test_root/etc/unionc"

if STAT_CONFIG_DIR=0:0:777 "$test_root/postinstall.sh" \
  >"$test_root/writable-config-dir.log" 2>&1; then
  fail 'postinstall trusted a writable Server config directory'
fi
if STAT_CONFIG_FILE=1000:1000:640 "$test_root/postinstall.sh" \
  >"$test_root/foreign-config-owner.log" 2>&1; then
  fail 'postinstall trusted a Server config not owned by root'
fi
if STAT_CONFIG_FILE=0:998:642 "$test_root/postinstall.sh" \
  >"$test_root/writable-config.log" 2>&1; then
  fail 'postinstall trusted a Server config writable by other users'
fi
if STAT_CONFIG_FILE_NLINK=2 "$test_root/postinstall.sh" \
  >"$test_root/hardlinked-config.log" 2>&1; then
  fail 'postinstall trusted a hard-linked Server config'
fi
if STAT_CONFIG_FILE=0:997:640 "$test_root/postinstall.sh" \
  >"$test_root/foreign-config-group.log" 2>&1; then
  fail 'postinstall trusted a Server config owned by an unrecorded group'
fi
if STAT_ACCOUNT_DATABASE_DIR=1000:1000:755 "$test_root/postinstall.sh" \
  >"$test_root/foreign-account-database-dir.log" 2>&1; then
  fail 'postinstall trusted an account database directory not owned by root'
fi
if STAT_ACCOUNT_DATABASE_DIR=0:0:777 "$test_root/postinstall.sh" \
  >"$test_root/writable-account-database-dir.log" 2>&1; then
  fail 'postinstall trusted a writable account database directory'
fi

# Fresh package extraction owns the config as root:root. The same checks must
# accept it and verify the transition to the recorded service group.
rm -f "$test_root/config-chowned-gid"
MODEL_CONFIG_CHOWN=1 STAT_CONFIG_FILE=0:0:640 "$test_root/postinstall.sh" \
  >"$test_root/fresh-config.log" 2>&1 ||
  fail 'postinstall rejected a fresh root-owned Server config'

reset_fresh_account_state() {
  rm -rf -- "$test_root/var/lib/unionc-package" "$test_root/var/lib/unionc"
  rm -f -- \
    "$test_root/group.created" "$test_root/group.deleted" \
    "$test_root/user.created" "$test_root/user.deleted" \
    "$test_root/group-enum-failed-once" "$test_root/passwd-enum-failed-once" \
    "$test_root/managed-group-sync-failed-once" \
    "$test_root/group-pending-cleanup-sync-failed-once" \
    "$test_root/group-account-sync-failed-once" \
    "$test_root/user-account-sync-failed-once" \
    "$test_root/group-shadow-missing" "$test_root/user-shadow-missing" \
    "$test_root/current-group-gid" "$test_root/current-user-uid" \
    "$test_root/current-user-gid" "$test_root/config-chowned-gid" \
    "$test_root/extra-group-entries" "$test_root/extra-passwd-entries"
  : >"$TEST_LOG"
}

assert_no_state_temporaries() {
  for marker_temporary in \
    "$test_root/var/lib/unionc-package"/.managed-* \
    "$test_root/var/lib/unionc-package"/.pending-*; do
    if [ -e "$marker_temporary" ] || [ -L "$marker_temporary" ]; then
      fail "unexpected marker temporary remains: $marker_temporary"
    fi
  done
}

assert_no_pending_markers() {
  for pending_marker in \
    "$test_root/var/lib/unionc-package/pending-group" \
    "$test_root/var/lib/unionc-package/pending-user"; do
    [ ! -e "$pending_marker" ] && [ ! -L "$pending_marker" ] ||
      fail "committed installation retained $pending_marker"
  done
}

command_count() {
  awk -v command_name="$1" '$1 == command_name { count += 1 } END { print count + 0 }' "$TEST_LOG"
}

# A durable pending intent is written before account creation. If marker
# publication fails before rename, the account is preserved and the next
# invocation completes the same identity without another create command.
reset_fresh_account_state
if START_ACCOUNTS_ABSENT=1 MODEL_CONFIG_CHOWN=1 STAT_CONFIG_FILE=0:0:640 \
  FAIL_GROUP_MARKER_MOVE=1 "$test_root/postinstall.sh" \
  >"$test_root/group-marker-failure.log" 2>&1; then
  fail 'postinstall accepted a failed managed-group marker publication'
fi
[ -f "$test_root/group.created" ] || fail 'group creation side effect was not modeled'
[ ! -f "$test_root/group.deleted" ] || fail 'postinstall deleted a recoverable pending group'
[ -f "$test_root/var/lib/unionc-package/pending-group" ] ||
  fail 'failed group marker publication did not preserve its pending intent'
[ ! -e "$test_root/var/lib/unionc-package/managed-group" ] ||
  fail 'failed group marker publication left a committed marker'
assert_no_state_temporaries
START_ACCOUNTS_ABSENT=1 MODEL_CONFIG_CHOWN=1 STAT_CONFIG_FILE=0:0:640 \
  "$test_root/postinstall.sh" >"$test_root/group-marker-retry.log" 2>&1 ||
  fail 'postinstall could not recover a group after marker publication failed'
[ "$(command_count groupadd)" -eq 1 ] ||
  fail 'group recovery repeated a creation command whose side effect already existed'
[ -f "$test_root/var/lib/unionc-package/managed-group" ] ||
  fail 'group marker retry did not commit the dedicated group'
[ -f "$test_root/var/lib/unionc-package/managed-user" ] ||
  fail 'group marker retry did not commit the dedicated user'
[ "$(sed -n 's/^gid=//p' "$test_root/var/lib/unionc-package/managed-group")" = 999 ] ||
  fail 'fresh group allocation did not use the highest free configured system gid'
[ "$(sed -n 's/^uid=//p' "$test_root/var/lib/unionc-package/managed-user")" = 999 ] ||
  fail 'fresh user allocation did not use the highest free configured system uid'
assert_no_pending_markers

reset_fresh_account_state
if START_ACCOUNTS_ABSENT=1 MODEL_CONFIG_CHOWN=1 STAT_CONFIG_FILE=0:0:640 \
  FAIL_USER_MARKER_MOVE=1 "$test_root/postinstall.sh" \
  >"$test_root/user-marker-failure.log" 2>&1; then
  fail 'postinstall accepted a failed managed-user marker publication'
fi
[ -f "$test_root/user.created" ] || fail 'user creation side effect was not modeled'
[ ! -f "$test_root/user.deleted" ] || fail 'postinstall deleted a recoverable pending user'
[ ! -e "$test_root/group.deleted" ] ||
  fail 'postinstall deleted a group whose marker was already committed'
[ -f "$test_root/var/lib/unionc-package/managed-group" ] ||
  fail 'user recovery discarded the committed group marker'
[ -f "$test_root/var/lib/unionc-package/pending-user" ] ||
  fail 'failed user marker publication did not preserve its pending intent'
[ ! -e "$test_root/var/lib/unionc-package/managed-user" ] ||
  fail 'failed user marker publication left a committed marker'
assert_no_state_temporaries
START_ACCOUNTS_ABSENT=1 MODEL_CONFIG_CHOWN=1 STAT_CONFIG_FILE=0:0:640 \
  "$test_root/postinstall.sh" >"$test_root/user-marker-retry.log" 2>&1 ||
  fail 'postinstall could not recover a user after marker publication failed'
[ "$(command_count useradd)" -eq 1 ] ||
  fail 'user recovery repeated a creation command whose side effect already existed'
[ -f "$test_root/var/lib/unionc-package/managed-user" ] ||
  fail 'user marker retry did not commit the dedicated user'
assert_no_pending_markers

# Pending evidence is bound to the preselected numeric identity. Replacing a
# same-name account between create and retry must fail closed instead of being
# adopted or deleted.
reset_fresh_account_state
if START_ACCOUNTS_ABSENT=1 MODEL_CONFIG_CHOWN=1 STAT_CONFIG_FILE=0:0:640 \
  FAIL_GROUP_MARKER_MOVE=1 REPLACE_GROUP_BEFORE_ROLLBACK=1 \
  "$test_root/postinstall.sh" >"$test_root/replaced-pending-group.log" 2>&1; then
  fail 'postinstall accepted a group replacement during marker publication'
fi
if START_ACCOUNTS_ABSENT=1 MODEL_CONFIG_CHOWN=1 STAT_CONFIG_FILE=0:0:640 \
  "$test_root/postinstall.sh" >"$test_root/replaced-pending-group-retry.log" 2>&1; then
  fail 'postinstall adopted a different gid on pending group recovery'
fi
[ -f "$test_root/var/lib/unionc-package/pending-group" ] ||
  fail 'group replacement discarded the original pending identity'
[ ! -e "$test_root/var/lib/unionc-package/managed-group" ] ||
  fail 'group replacement was committed under the original pending intent'
[ ! -e "$test_root/group.deleted" ] || fail 'group replacement was deleted'
[ "$(command_count groupadd)" -eq 1 ] || fail 'group replacement recovery repeated groupadd'

reset_fresh_account_state
if START_ACCOUNTS_ABSENT=1 MODEL_CONFIG_CHOWN=1 STAT_CONFIG_FILE=0:0:640 \
  FAIL_USER_MARKER_MOVE=1 REPLACE_USER_BEFORE_ROLLBACK=1 \
  "$test_root/postinstall.sh" >"$test_root/replaced-pending-user.log" 2>&1; then
  fail 'postinstall accepted a user replacement during marker publication'
fi
if START_ACCOUNTS_ABSENT=1 MODEL_CONFIG_CHOWN=1 STAT_CONFIG_FILE=0:0:640 \
  "$test_root/postinstall.sh" >"$test_root/replaced-pending-user-retry.log" 2>&1; then
  fail 'postinstall adopted a different uid on pending user recovery'
fi
[ -f "$test_root/var/lib/unionc-package/managed-group" ] ||
  fail 'user replacement lost the previously committed group'
[ -f "$test_root/var/lib/unionc-package/pending-user" ] ||
  fail 'user replacement discarded the original pending identity'
[ ! -e "$test_root/var/lib/unionc-package/managed-user" ] ||
  fail 'user replacement was committed under the original pending intent'
[ ! -e "$test_root/user.deleted" ] || fail 'user replacement was deleted'
[ "$(command_count useradd)" -eq 1 ] || fail 'user replacement recovery repeated useradd'

# The account database can live on a different filesystem from package state.
# Its durability barrier must succeed before a committed marker is published.
reset_fresh_account_state
if START_ACCOUNTS_ABSENT=1 MODEL_CONFIG_CHOWN=1 STAT_CONFIG_FILE=0:0:640 \
  FAIL_GROUP_ACCOUNT_SYNC_ONCE=1 "$test_root/postinstall.sh" \
  >"$test_root/group-account-sync-failure.log" 2>&1; then
  fail 'postinstall continued after the group account durability barrier failed'
fi
[ -f "$test_root/group.created" ] || fail 'group account sync fixture did not create the group'
[ -f "$test_root/var/lib/unionc-package/pending-group" ] ||
  fail 'group account sync failure lost its pending intent'
[ ! -e "$test_root/var/lib/unionc-package/managed-group" ] ||
  fail 'group marker was published before the account durability barrier'
START_ACCOUNTS_ABSENT=1 MODEL_CONFIG_CHOWN=1 STAT_CONFIG_FILE=0:0:640 \
  "$test_root/postinstall.sh" >"$test_root/group-account-sync-retry.log" 2>&1 ||
  fail 'postinstall could not recover after the group account barrier resumed'
[ "$(command_count groupadd)" -eq 1 ] || fail 'group account sync recovery repeated groupadd'
assert_no_pending_markers

reset_fresh_account_state
if START_ACCOUNTS_ABSENT=1 MODEL_CONFIG_CHOWN=1 STAT_CONFIG_FILE=0:0:640 \
  FAIL_USER_ACCOUNT_SYNC_ONCE=1 "$test_root/postinstall.sh" \
  >"$test_root/user-account-sync-failure.log" 2>&1; then
  fail 'postinstall continued after the user account durability barrier failed'
fi
[ -f "$test_root/user.created" ] || fail 'user account sync fixture did not create the user'
[ -f "$test_root/var/lib/unionc-package/pending-user" ] ||
  fail 'user account sync failure lost its pending intent'
[ ! -e "$test_root/var/lib/unionc-package/managed-user" ] ||
  fail 'user marker was published before the account durability barrier'
START_ACCOUNTS_ABSENT=1 MODEL_CONFIG_CHOWN=1 STAT_CONFIG_FILE=0:0:640 \
  "$test_root/postinstall.sh" >"$test_root/user-account-sync-retry.log" 2>&1 ||
  fail 'postinstall could not recover after the user account barrier resumed'
[ "$(command_count useradd)" -eq 1 ] || fail 'user account sync recovery repeated useradd'
assert_no_pending_markers

# Durability barriers are part of the state transition. Temporary-file and
# pending-directory sync failures must stop before account creation; marker and
# cleanup sync failures remain recoverable without repeating groupadd.
reset_fresh_account_state
if START_ACCOUNTS_ABSENT=1 MODEL_CONFIG_CHOWN=1 STAT_CONFIG_FILE=0:0:640 \
  FAIL_PENDING_GROUP_TEMP_SYNC=1 "$test_root/postinstall.sh" \
  >"$test_root/pending-group-temp-sync-failure.log" 2>&1; then
  fail 'postinstall continued after the pending-group temporary sync failed'
fi
[ -f "$test_root/var/lib/unionc-package/.pending-group.new" ] ||
  fail 'temporary sync fixture did not leave an interrupted state file'
[ ! -e "$test_root/var/lib/unionc-package/pending-group" ] ||
  fail 'unsynced pending-group temporary was published'
[ "$(command_count groupadd)" -eq 0 ] ||
  fail 'pending-group temporary sync failure reached groupadd'
START_ACCOUNTS_ABSENT=1 MODEL_CONFIG_CHOWN=1 STAT_CONFIG_FILE=0:0:640 \
  "$test_root/postinstall.sh" >"$test_root/pending-group-temp-sync-retry.log" 2>&1 ||
  fail 'postinstall could not discard an interrupted state temporary and retry'
[ "$(command_count groupadd)" -eq 1 ] || fail 'temporary cleanup retry used an unexpected groupadd count'
assert_no_state_temporaries
assert_no_pending_markers

reset_fresh_account_state
if START_ACCOUNTS_ABSENT=1 MODEL_CONFIG_CHOWN=1 STAT_CONFIG_FILE=0:0:640 \
  FAIL_PENDING_GROUP_DIR_SYNC=1 "$test_root/postinstall.sh" \
  >"$test_root/pending-group-sync-failure.log" 2>&1; then
  fail 'postinstall continued after the pending-group durability barrier failed'
fi
[ -f "$test_root/var/lib/unionc-package/pending-group" ] ||
  fail 'pending-group sync failure lost the live recovery intent'
[ ! -f "$test_root/group.created" ] ||
  fail 'groupadd ran before pending-group directory sync completed'
[ "$(command_count groupadd)" -eq 0 ] ||
  fail 'pending-group directory sync failure reached groupadd'
START_ACCOUNTS_ABSENT=1 MODEL_CONFIG_CHOWN=1 STAT_CONFIG_FILE=0:0:640 \
  "$test_root/postinstall.sh" >"$test_root/pending-group-sync-retry.log" 2>&1 ||
  fail 'postinstall could not retry after pending-group sync recovered'
[ "$(command_count groupadd)" -eq 1 ] || fail 'pending sync recovery used an unexpected groupadd count'
assert_no_pending_markers

reset_fresh_account_state
if START_ACCOUNTS_ABSENT=1 MODEL_CONFIG_CHOWN=1 STAT_CONFIG_FILE=0:0:640 \
  FAIL_MANAGED_GROUP_DIR_SYNC_ONCE=1 "$test_root/postinstall.sh" \
  >"$test_root/managed-group-sync-failure.log" 2>&1; then
  fail 'postinstall continued after the managed-group durability barrier failed'
fi
[ -f "$test_root/var/lib/unionc-package/managed-group" ] ||
  fail 'managed-group sync fixture did not expose the renamed marker'
[ -f "$test_root/var/lib/unionc-package/pending-group" ] ||
  fail 'managed-group sync failure removed pending recovery too early'
START_ACCOUNTS_ABSENT=1 MODEL_CONFIG_CHOWN=1 STAT_CONFIG_FILE=0:0:640 \
  "$test_root/postinstall.sh" >"$test_root/managed-group-sync-retry.log" 2>&1 ||
  fail 'postinstall could not recover a marker after directory sync resumed'
[ "$(command_count groupadd)" -eq 1 ] || fail 'managed marker sync recovery repeated groupadd'
assert_no_pending_markers

reset_fresh_account_state
if START_ACCOUNTS_ABSENT=1 MODEL_CONFIG_CHOWN=1 STAT_CONFIG_FILE=0:0:640 \
  FAIL_GROUP_PENDING_CLEANUP_SYNC_ONCE=1 "$test_root/postinstall.sh" \
  >"$test_root/group-pending-cleanup-sync-failure.log" 2>&1; then
  fail 'postinstall ignored a failed pending-group cleanup barrier'
fi
[ -f "$test_root/var/lib/unionc-package/managed-group" ] ||
  fail 'pending cleanup sync failure lost the committed group marker'
[ ! -e "$test_root/var/lib/unionc-package/pending-group" ] ||
  fail 'pending cleanup sync fixture did not remove the live directory entry'
START_ACCOUNTS_ABSENT=1 MODEL_CONFIG_CHOWN=1 STAT_CONFIG_FILE=0:0:640 \
  "$test_root/postinstall.sh" >"$test_root/group-pending-cleanup-sync-retry.log" 2>&1 ||
  fail 'postinstall could not retry after pending cleanup sync recovered'
[ "$(command_count groupadd)" -eq 1 ] || fail 'pending cleanup sync recovery repeated groupadd'
assert_no_pending_markers

# The account-management exit code is not the commit fact. A side effect that
# accompanies a failure status is recovered immediately from NSS and committed.
reset_fresh_account_state
if START_ACCOUNTS_ABSENT=1 MODEL_CONFIG_CHOWN=1 STAT_CONFIG_FILE=0:0:640 \
  GROUPADD_PUBLIC_ONLY=1 "$test_root/postinstall.sh" \
  >"$test_root/groupadd-public-only.log" 2>&1; then
  fail 'postinstall committed a group whose gshadow record was missing'
fi
[ -f "$test_root/var/lib/unionc-package/pending-group" ] ||
  fail 'partial group creation lost the pending intent'
[ ! -e "$test_root/var/lib/unionc-package/managed-group" ] ||
  fail 'partial group creation published a managed marker'
if START_ACCOUNTS_ABSENT=1 MODEL_CONFIG_CHOWN=1 STAT_CONFIG_FILE=0:0:640 \
  "$test_root/postinstall.sh" >"$test_root/groupadd-public-only-retry.log" 2>&1; then
  fail 'postinstall committed a group while gshadow was still missing'
fi
[ "$(command_count groupadd)" -eq 1 ] || fail 'partial group recovery repeated groupadd'
rm -f "$test_root/group-shadow-missing"
START_ACCOUNTS_ABSENT=1 MODEL_CONFIG_CHOWN=1 STAT_CONFIG_FILE=0:0:640 \
  "$test_root/postinstall.sh" >"$test_root/groupadd-shadow-repaired.log" 2>&1 ||
  fail 'postinstall could not finish after the exact gshadow record was repaired'
[ "$(command_count groupadd)" -eq 1 ] || fail 'gshadow repair recovery repeated groupadd'
assert_no_pending_markers

reset_fresh_account_state
if START_ACCOUNTS_ABSENT=1 MODEL_CONFIG_CHOWN=1 STAT_CONFIG_FILE=0:0:640 \
  USERADD_PUBLIC_ONLY=1 "$test_root/postinstall.sh" \
  >"$test_root/useradd-public-only.log" 2>&1; then
  fail 'postinstall committed a user whose shadow record was missing'
fi
[ -f "$test_root/var/lib/unionc-package/managed-group" ] ||
  fail 'partial user creation lost the committed group'
[ -f "$test_root/var/lib/unionc-package/pending-user" ] ||
  fail 'partial user creation lost the pending intent'
[ ! -e "$test_root/var/lib/unionc-package/managed-user" ] ||
  fail 'partial user creation published a managed marker'
if START_ACCOUNTS_ABSENT=1 MODEL_CONFIG_CHOWN=1 STAT_CONFIG_FILE=0:0:640 \
  "$test_root/postinstall.sh" >"$test_root/useradd-public-only-retry.log" 2>&1; then
  fail 'postinstall committed a user while shadow was still missing'
fi
[ "$(command_count useradd)" -eq 1 ] || fail 'partial user recovery repeated useradd'
rm -f "$test_root/user-shadow-missing"
START_ACCOUNTS_ABSENT=1 MODEL_CONFIG_CHOWN=1 STAT_CONFIG_FILE=0:0:640 \
  "$test_root/postinstall.sh" >"$test_root/useradd-shadow-repaired.log" 2>&1 ||
  fail 'postinstall could not finish after the exact shadow record was repaired'
[ "$(command_count useradd)" -eq 1 ] || fail 'shadow repair recovery repeated useradd'
assert_no_pending_markers

reset_fresh_account_state
START_ACCOUNTS_ABSENT=1 MODEL_CONFIG_CHOWN=1 STAT_CONFIG_FILE=0:0:640 \
  FAIL_GROUPADD_AFTER_CREATE=1 "$test_root/postinstall.sh" \
  >"$test_root/groupadd-side-effect-error.log" 2>&1 ||
  fail 'postinstall discarded a groupadd side effect after a failure status'
[ "$(command_count groupadd)" -eq 1 ] || fail 'groupadd side-effect recovery retried creation'
assert_no_pending_markers

reset_fresh_account_state
START_ACCOUNTS_ABSENT=1 MODEL_CONFIG_CHOWN=1 STAT_CONFIG_FILE=0:0:640 \
  FAIL_USERADD_AFTER_CREATE=1 "$test_root/postinstall.sh" \
  >"$test_root/useradd-side-effect-error.log" 2>&1 ||
  fail 'postinstall discarded a useradd side effect after a failure status'
[ "$(command_count useradd)" -eq 1 ] || fail 'useradd side-effect recovery retried creation'
assert_no_pending_markers

# A failed create with no side effect keeps only the intent. Retrying invokes
# the create exactly once more and then removes the pending journal.
reset_fresh_account_state
if START_ACCOUNTS_ABSENT=1 MODEL_CONFIG_CHOWN=1 STAT_CONFIG_FILE=0:0:640 \
  FAIL_GROUPADD_WITHOUT_CREATE=1 "$test_root/postinstall.sh" \
  >"$test_root/groupadd-no-side-effect.log" 2>&1; then
  fail 'postinstall accepted groupadd failure without an account'
fi
[ -f "$test_root/var/lib/unionc-package/pending-group" ] ||
  fail 'groupadd failure without a side effect lost the pending intent'
[ ! -f "$test_root/group.created" ] || fail 'groupadd no-side-effect fixture created a group'
START_ACCOUNTS_ABSENT=1 MODEL_CONFIG_CHOWN=1 STAT_CONFIG_FILE=0:0:640 \
  "$test_root/postinstall.sh" >"$test_root/groupadd-no-side-effect-retry.log" 2>&1 ||
  fail 'postinstall could not retry groupadd after a no-side-effect failure'
[ "$(command_count groupadd)" -eq 2 ] || fail 'groupadd no-side-effect recovery used an unexpected call count'
assert_no_pending_markers

reset_fresh_account_state
if START_ACCOUNTS_ABSENT=1 MODEL_CONFIG_CHOWN=1 STAT_CONFIG_FILE=0:0:640 \
  FAIL_USERADD_WITHOUT_CREATE=1 "$test_root/postinstall.sh" \
  >"$test_root/useradd-no-side-effect.log" 2>&1; then
  fail 'postinstall accepted useradd failure without an account'
fi
[ -f "$test_root/var/lib/unionc-package/managed-group" ] ||
  fail 'useradd failure lost the already committed group'
[ -f "$test_root/var/lib/unionc-package/pending-user" ] ||
  fail 'useradd failure without a side effect lost the pending intent'
START_ACCOUNTS_ABSENT=1 MODEL_CONFIG_CHOWN=1 STAT_CONFIG_FILE=0:0:640 \
  "$test_root/postinstall.sh" >"$test_root/useradd-no-side-effect-retry.log" 2>&1 ||
  fail 'postinstall could not retry useradd after a no-side-effect failure'
[ "$(command_count useradd)" -eq 2 ] || fail 'useradd no-side-effect recovery used an unexpected call count'
assert_no_pending_markers

# A concurrent allocation of the preselected numeric ID must not be mistaken
# for the package account, even though the pending intent itself is valid.
reset_fresh_account_state
if START_ACCOUNTS_ABSENT=1 MODEL_CONFIG_CHOWN=1 STAT_CONFIG_FILE=0:0:640 \
  OCCUPY_GROUP_GID_BEFORE_CREATE=1 "$test_root/postinstall.sh" \
  >"$test_root/occupied-group-gid.log" 2>&1; then
  fail 'postinstall accepted another group occupying the pending gid'
fi
pending_gid=$(sed -n 's/^gid=//p' "$test_root/var/lib/unionc-package/pending-group")
grep -Fx "other:x:$pending_gid:" "$test_root/extra-group-entries" >/dev/null ||
  fail 'group gid collision fixture did not occupy the selected gid'
[ ! -e "$test_root/var/lib/unionc-package/managed-group" ] ||
  fail 'another group occupying the selected gid was committed'
[ ! -f "$test_root/group.created" ] || fail 'gid collision created the unionc group'

reset_fresh_account_state
if START_ACCOUNTS_ABSENT=1 MODEL_CONFIG_CHOWN=1 STAT_CONFIG_FILE=0:0:640 \
  OCCUPY_USER_UID_BEFORE_CREATE=1 "$test_root/postinstall.sh" \
  >"$test_root/occupied-user-uid.log" 2>&1; then
  fail 'postinstall accepted another user occupying the pending uid'
fi
pending_uid=$(sed -n 's/^uid=//p' "$test_root/var/lib/unionc-package/pending-user")
grep -F "other:x:$pending_uid:" "$test_root/extra-passwd-entries" >/dev/null ||
  fail 'user uid collision fixture did not occupy the selected uid'
[ ! -e "$test_root/var/lib/unionc-package/managed-user" ] ||
  fail 'another user occupying the selected uid was committed'
[ ! -f "$test_root/user.created" ] || fail 'uid collision created the unionc user'

# If the first NSS enumeration after create is unavailable, a later package
# invocation adopts only the exact identity bound to the persisted intent.
reset_fresh_account_state
if START_ACCOUNTS_ABSENT=1 MODEL_CONFIG_CHOWN=1 STAT_CONFIG_FILE=0:0:640 \
  FAIL_GROUP_ENUM_ONCE_AFTER_CREATE=1 "$test_root/postinstall.sh" \
  >"$test_root/group-enum-interruption.log" 2>&1; then
  fail 'postinstall ignored a failed post-groupadd NSS enumeration'
fi
[ -f "$test_root/group.created" ] || fail 'group enumeration fixture did not create the group'
[ -f "$test_root/var/lib/unionc-package/pending-group" ] ||
  fail 'group enumeration failure lost the pending intent'
START_ACCOUNTS_ABSENT=1 MODEL_CONFIG_CHOWN=1 STAT_CONFIG_FILE=0:0:640 \
  "$test_root/postinstall.sh" >"$test_root/group-enum-interruption-retry.log" 2>&1 ||
  fail 'postinstall could not recover after group enumeration resumed'
[ "$(command_count groupadd)" -eq 1 ] || fail 'group NSS recovery repeated groupadd'
assert_no_pending_markers

reset_fresh_account_state
if START_ACCOUNTS_ABSENT=1 MODEL_CONFIG_CHOWN=1 STAT_CONFIG_FILE=0:0:640 \
  FAIL_PASSWD_ENUM_ONCE_AFTER_CREATE=1 "$test_root/postinstall.sh" \
  >"$test_root/user-enum-interruption.log" 2>&1; then
  fail 'postinstall ignored a failed post-useradd NSS enumeration'
fi
[ -f "$test_root/user.created" ] || fail 'user enumeration fixture did not create the user'
[ -f "$test_root/var/lib/unionc-package/pending-user" ] ||
  fail 'user enumeration failure lost the pending intent'
START_ACCOUNTS_ABSENT=1 MODEL_CONFIG_CHOWN=1 STAT_CONFIG_FILE=0:0:640 \
  "$test_root/postinstall.sh" >"$test_root/user-enum-interruption-retry.log" 2>&1 ||
  fail 'postinstall could not recover after user enumeration resumed'
[ "$(command_count useradd)" -eq 1 ] || fail 'user NSS recovery repeated useradd'
assert_no_pending_markers

# A rename wrapper can report failure after the destination already exists.
# The strictly re-read marker wins, so the install completes without rollback.
reset_fresh_account_state
START_ACCOUNTS_ABSENT=1 MODEL_CONFIG_CHOWN=1 STAT_CONFIG_FILE=0:0:640 \
  FAIL_GROUP_MARKER_MOVE_AFTER_RENAME=1 "$test_root/postinstall.sh" \
  >"$test_root/group-marker-ambiguous-rename.log" 2>&1 ||
  fail 'postinstall ignored a valid group marker after ambiguous rename status'
assert_no_pending_markers

reset_fresh_account_state
START_ACCOUNTS_ABSENT=1 MODEL_CONFIG_CHOWN=1 STAT_CONFIG_FILE=0:0:640 \
  FAIL_USER_MARKER_MOVE_AFTER_RENAME=1 "$test_root/postinstall.sh" \
  >"$test_root/user-marker-ambiguous-rename.log" 2>&1 ||
  fail 'postinstall ignored a valid user marker after ambiguous rename status'
assert_no_pending_markers

# A crash after marker publication but before pending cleanup leaves both
# files. A valid committed marker is authoritative and stale intent is removed.
stale_group_gid=$(sed -n 's/^gid=//p' "$test_root/var/lib/unionc-package/managed-group")
stale_user_uid=$(sed -n 's/^uid=//p' "$test_root/var/lib/unionc-package/managed-user")
stale_user_gid=$(sed -n 's/^primary_gid=//p' "$test_root/var/lib/unionc-package/managed-user")
cat >"$test_root/var/lib/unionc-package/pending-group" <<EOF
format=$package_version
state=pending
kind=group
name=unionc
gid=$stale_group_gid
EOF
cat >"$test_root/var/lib/unionc-package/pending-user" <<EOF
format=$package_version
state=pending
kind=user
name=unionc
uid=$stale_user_uid
primary_gid=$stale_user_gid
home=$test_root/var/lib/unionc
shell=/usr/sbin/nologin
EOF
/usr/bin/chmod 0600 \
  "$test_root/var/lib/unionc-package/pending-group" \
  "$test_root/var/lib/unionc-package/pending-user"
: >"$TEST_LOG"
START_ACCOUNTS_ABSENT=1 MODEL_CONFIG_CHOWN=1 "$test_root/postinstall.sh" \
  >"$test_root/stale-pending-recovery.log" 2>&1 || {
  cat "$test_root/stale-pending-recovery.log" >&2
  fail 'postinstall could not clean pending intents after committed markers'
}
assert_no_pending_markers
[ "$(command_count groupadd)" -eq 0 ] || fail 'stale group pending cleanup recreated the group'
[ "$(command_count useradd)" -eq 0 ] || fail 'stale user pending cleanup recreated the user'

# login.defs defaults and ambiguity are part of deterministic preallocation.
reset_fresh_account_state
cat >"$test_root/etc/login.defs" <<'EOF'
UID_MIN 1000
GID_MIN 1000
EOF
START_ACCOUNTS_ABSENT=1 MODEL_CONFIG_CHOWN=1 STAT_CONFIG_FILE=0:0:640 \
  "$test_root/postinstall.sh" >"$test_root/default-system-id-range.log" 2>&1 ||
  fail 'postinstall could not use the documented default system identity ranges'
[ "$(sed -n 's/^gid=//p' "$test_root/var/lib/unionc-package/managed-group")" = 999 ] ||
  fail 'default SYS_GID range did not end at GID_MIN minus one'
[ "$(sed -n 's/^uid=//p' "$test_root/var/lib/unionc-package/managed-user")" = 999 ] ||
  fail 'default SYS_UID range did not end at UID_MIN minus one'
write_test_login_defs

reset_fresh_account_state
cat >"$test_root/etc/login.defs" <<'EOF'
UID_MIN 1000
GID_MIN 1000
SYS_UID_MIN 900
SYS_UID_MIN 901
SYS_UID_MAX 999
SYS_GID_MIN 900
SYS_GID_MAX 999
EOF
if START_ACCOUNTS_ABSENT=1 MODEL_CONFIG_CHOWN=1 STAT_CONFIG_FILE=0:0:640 \
  "$test_root/postinstall.sh" >"$test_root/ambiguous-system-id-range.log" 2>&1; then
  fail 'postinstall accepted duplicate system identity range definitions'
fi
[ "$(command_count groupadd)" -eq 0 ] || fail 'ambiguous identity ranges reached groupadd'
[ "$(command_count useradd)" -eq 0 ] || fail 'ambiguous identity ranges reached useradd'
write_test_login_defs

# Malformed recovery evidence is rejected before any account-management call.
reset_fresh_account_state
mkdir -p "$test_root/var/lib/unionc-package"
cat >"$test_root/var/lib/unionc-package/pending-group" <<EOF
format=$package_version
state=pending
kind=group
name=unionc
gid=999
unknown=field
EOF
/usr/bin/chmod 0600 "$test_root/var/lib/unionc-package/pending-group"
if START_ACCOUNTS_ABSENT=1 MODEL_CONFIG_CHOWN=1 STAT_CONFIG_FILE=0:0:640 \
  "$test_root/postinstall.sh" >"$test_root/malformed-pending.log" 2>&1; then
  fail 'postinstall accepted a malformed pending group marker'
fi
[ "$(command_count groupadd)" -eq 0 ] || fail 'malformed pending evidence reached groupadd'
[ "$(command_count useradd)" -eq 0 ] || fail 'malformed pending evidence reached useradd'

echo 'Server Linux lifecycle checks passed'
