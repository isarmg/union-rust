#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
packaging_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)
test_root=$(mktemp -d "${TMPDIR:-/tmp}/unionc-agent-packaging-test.XXXXXX")
package_version=0.3.2
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
    -e "s#__PACKAGE_STATE__#$test_root/var/lib/unionc-agent-package#g" \
    -e "s#__AGENT_STATE__#$test_root/var/lib/unionc-agent#g" \
    -e "s#__DROPIN_DIR__#$test_root/etc/systemd/system/unionc-agent.service.d#g" \
    -e "s#__CONFIG_DIR__#$test_root/etc/unionc-agent#g" \
    -e "s#__SYSTEMD_RUNTIME__#$test_root/run/systemd/system#g" \
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
  rewrite_for_test_root "$source_script" "$test_root/$(basename "$source_script")"
done

mkdir -p "$test_root/bin" "$test_root/run/systemd/system"
TEST_LOG="$test_root/commands.log"
export TEST_LOG test_root
: >"$TEST_LOG"

cat >"$test_root/bin/systemctl" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >>"$TEST_LOG"
case "$1" in
  show)
    echo loaded
    ;;
  restart)
    [ "${FAIL_RESTART:-0}" -ne 1 ]
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
if [ "$owner" = root:root ] &&
  [ "$destination" = "$test_root/var/lib/unionc-agent-package/config.json.remove-backup" ]; then
  : >"$test_root/backup.root-owned"
fi
exit 0
EOF

cat >"$test_root/bin/mv" <<'EOF'
#!/bin/sh
destination=
for argument in "$@"; do
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
esac
exec /usr/bin/mv "$@"
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
  "$test_root/etc/unionc-agent/config.json")
    if [ -f "$test_root/var/lib/unionc-agent-package/managed-group" ]; then
      metadata=${STAT_CONFIG_FILE:-0:${AGENT_GID:-998}:640}
    else
      metadata=${STAT_CONFIG_FILE:-0:0:600}
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
    "$test_root/backup.root-owned" "$test_root/current-user-uid" \
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
    "$test_root/backup.root-owned" "$test_root/current-user-uid" \
    "$test_root/current-group-gid"
  write_package_config
}

# Debian exposes same-version reinstall through its `upgrade` script ABI. The
# current package is accepted, while a different package version fails closed.
: >"$TEST_LOG"
"$test_root/preremove.sh" upgrade "$package_version"
assert_log_contains 'stop unionc-agent.service'
if "$test_root/preremove.sh" upgrade 0.3.1 >/dev/null 2>&1; then
  fail 'Debian cross-version replacement was accepted'
fi

# RPM replacement runs pre-remove after current postinstall. A
# positive remaining-instance count must not disable the validated 0.3.2 service.
: >"$TEST_LOG"
"$test_root/preremove.sh" 1
[ ! -s "$TEST_LOG" ] || fail 'RPM same-version reinstall stopped the current service'

# Current remove/reinstall protects the config before payload removal and
# restores it afterwards.
mkdir -p "$test_root/etc/unionc-agent" \
  "$test_root/var/lib/unionc-agent" \
  "$test_root/etc/systemd/system/unionc-agent.service.d"
echo retained-config >"$test_root/etc/unionc-agent/config.json"
echo retained-state >"$test_root/var/lib/unionc-agent/agent-token"
echo retained-dropin >"$test_root/etc/systemd/system/unionc-agent.service.d/gpu.conf"
: >"$TEST_LOG"
"$test_root/preremove.sh" 0
assert_exists "$test_root/var/lib/unionc-agent-package/config.json.remove-backup"
assert_log_contains 'disable --now unionc-agent.service'
rm -f "$test_root/etc/unionc-agent/config.json"
"$test_root/postremove.sh" 0
assert_exists "$test_root/etc/unionc-agent/config.json"
grep -F retained-config "$test_root/etc/unionc-agent/config.json" >/dev/null ||
  fail 'RPM config content was not preserved'
assert_exists "$test_root/var/lib/unionc-agent/agent-token"
assert_exists "$test_root/etc/systemd/system/unionc-agent.service.d/gpu.conf"
assert_log_contains 'daemon-reload'

# Debian remove preserves all local state and does not touch the account.
: >"$TEST_LOG"
"$test_root/postremove.sh" remove
assert_exists "$test_root/etc/unionc-agent/config.json"
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

# A creation-time numeric marker prevents a later same-name account from being
# mistaken for the package-created identity.
rm -f "$test_root/user.deleted" "$test_root/group.deleted"
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
if grep -F '后台服务已启用并正在运行' "$test_root/postinstall-failure.log" >/dev/null; then
  fail 'postinstall printed a false success message'
fi
assert_exists "$test_root/var/lib/unionc-agent-package/managed-user"
assert_exists "$test_root/var/lib/unionc-agent-package/managed-group"

echo 'Linux packaging lifecycle tests passed'
