#!/bin/sh
set -eu

script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
postinstall="$script_dir/../scripts/postinstall"
preinstall="$script_dir/../scripts/preinstall"
test_root="$(mktemp -d "${TMPDIR:-/tmp}/unionc-macos-postinstall.XXXXXX")"

cleanup() {
  rm -rf "$test_root"
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM

fail() {
  echo "macOS postinstall failure test: $*" >&2
  exit 1
}

make_case() {
  case_dir="$1"
  case_bin="$case_dir/bin"
  case_state="$case_dir/Library/Application Support/UnionC Agent"
  case_ownership="$case_dir/var/db/unionc-agent"
  case_log="$case_dir/var/log/unionc-agent.log"
  case_agent_binary="$case_dir/usr/local/libexec/unionc-agent"
  case_agent_command="$case_dir/usr/local/bin/unionc-agent"
  case_logrotate_helper="$case_dir/usr/local/libexec/unionc-agent-logrotate"
  case_agent_plist="$case_dir/Library/LaunchDaemons/com.unionc.agent.plist"
  case_logrotate_plist="$case_dir/Library/LaunchDaemons/com.unionc.agent.logrotate.plist"
  mkdir -p "$case_bin" "$case_state" "$case_dir/dscl/Groups" "$case_dir/dscl/Users" \
    "$case_dir/launch" "$(dirname -- "$case_log")" "$(dirname -- "$case_agent_plist")" \
    "$(dirname -- "$case_agent_binary")" "$(dirname -- "$case_agent_command")"
  {
    printf '{\n'
    printf '  "application_version": "@UNIONC_AGENT_PACKAGE_VERSION@",\n'
    printf '  "server_url": null\n'
    printf '}\n'
  } >"$case_state/config.example.json"
  cat >"$case_agent_binary" <<'EOF'
#!/bin/sh
[ "${1:-}" = --version ] || exit 64
printf 'unionc-agent @UNIONC_AGENT_PACKAGE_VERSION@\n'
EOF
  cat >"$case_logrotate_helper" <<'EOF'
#!/bin/sh
exit 0
EOF
  cat >"$case_agent_plist" <<'EOF'
<?xml version="1.0"?><plist version="1.0"><dict><key>Label</key><string>com.unionc.agent</string></dict></plist>
EOF
  cat >"$case_logrotate_plist" <<'EOF'
<?xml version="1.0"?><plist version="1.0"><dict><key>Label</key><string>com.unionc.agent.logrotate</string></dict></plist>
EOF
  chmod 0755 "$case_agent_binary" "$case_logrotate_helper"
  ln -s ../libexec/unionc-agent "$case_agent_command"

  sed \
    -e "s|^PATH=.*|PATH=\"$case_bin:/bin:/usr/bin:/sbin:/usr/sbin\"|" \
    -e "s|^state=.*|state=\"$case_state\"|" \
    -e "s|^log=.*|log=\"$case_log\"|" \
    -e "s|^agent_binary=.*|agent_binary=\"$case_agent_binary\"|" \
    -e "s|^agent_command=.*|agent_command=\"$case_agent_command\"|" \
    -e "s|^logrotate_helper=.*|logrotate_helper=\"$case_logrotate_helper\"|" \
    -e "s|^agent_plist=.*|agent_plist=\"$case_agent_plist\"|" \
    -e "s|^logrotate_plist=.*|logrotate_plist=\"$case_logrotate_plist\"|" \
    -e "s|^ownership_dir=.*|ownership_dir=\"$case_ownership\"|" \
    "$postinstall" >"$case_dir/postinstall"
  chmod +x "$case_dir/postinstall"

  sed \
    -e "s|^PATH=.*|PATH=\"$case_bin:/bin:/usr/bin:/sbin:/usr/sbin\"|" \
    -e "s|^agent_plist=.*|agent_plist=\"$case_agent_plist\"|" \
    -e "s|^logrotate_plist=.*|logrotate_plist=\"$case_logrotate_plist\"|" \
    -e "s|^ownership_dir=.*|ownership_dir=\"$case_ownership\"|" \
    "$preinstall" >"$case_dir/preinstall"
  chmod +x "$case_dir/preinstall"

  cat >"$case_bin/id" <<'EOF'
#!/bin/sh
if [ "${1:-}" = -u ]; then
  printf '0\n'
  exit 0
fi
exec /usr/bin/id "$@"
EOF

  cat >"$case_bin/install" <<'EOF'
#!/bin/sh
destination=
for argument do
  destination="$argument"
done
[ -n "$destination" ] || exit 64
mkdir -p "$destination"
EOF

  cat >"$case_bin/stat" <<'EOF'
#!/bin/sh
format=
path=
while [ "$#" -gt 0 ]; do
  case "$1" in
    -f)
      shift
      format="$1"
      ;;
    *) path="$1" ;;
  esac
  shift
done
[ "$format" = '%u:%g:%Mp:%Lp' ] || exit 64
case "$path" in
  "$CASE_ROOT/usr/local/libexec/unionc-agent")
    metadata="${STAT_AGENT_BINARY:-0:0:755}"
    ;;
  "$CASE_ROOT/usr/local/bin/unionc-agent")
    metadata="${STAT_AGENT_COMMAND:-0:0:755}"
    ;;
  "$CASE_ROOT/usr/local/libexec/unionc-agent-logrotate")
    metadata="${STAT_LOGROTATE_HELPER:-0:0:755}"
    ;;
  "$CASE_ROOT/Library/LaunchDaemons/com.unionc.agent.plist")
    metadata="${STAT_AGENT_PLIST:-0:0:644}"
    ;;
  "$CASE_ROOT/Library/LaunchDaemons/com.unionc.agent.logrotate.plist")
    metadata="${STAT_LOGROTATE_PLIST:-0:0:644}"
    ;;
  "$CASE_ROOT/var/db/unionc-agent")
    metadata="${STAT_OWNERSHIP_DIR:-0:0:700}"
    ;;
  "$CASE_ROOT/var/db/unionc-agent/account-ownership"|\
  "$CASE_ROOT/var/db/unionc-agent/install-recovery")
    metadata="${STAT_OWNERSHIP_MARKER:-0:0:600}"
    ;;
  "$CASE_ROOT/Library/Application Support/UnionC Agent")
    if grep -Fx 'user_created=1' "$CASE_ROOT/var/db/unionc-agent/account-ownership" >/dev/null 2>&1; then
      metadata="${STAT_STATE_ROOT:-450:450:700}"
    else
      metadata="${STAT_STATE_ROOT:-0:0:755}"
    fi
    ;;
  "$CASE_ROOT/Library/Application Support/UnionC Agent/config.example.json")
    if grep -Fx 'user_created=1' "$CASE_ROOT/var/db/unionc-agent/account-ownership" >/dev/null 2>&1; then
      metadata="${STAT_CONFIG_EXAMPLE:-450:450:600}"
    else
      metadata="${STAT_CONFIG_EXAMPLE:-0:0:600}"
    fi
    ;;
  "$CASE_ROOT/Library/Application Support/UnionC Agent/config.json")
    if grep -Fx 'user_created=1' "$CASE_ROOT/var/db/unionc-agent/account-ownership" >/dev/null 2>&1; then
      metadata="${STAT_CONFIG:-450:450:600}"
    else
      metadata="${STAT_CONFIG:-0:0:600}"
    fi
    ;;
  "$CASE_ROOT/var/log")
    metadata="${STAT_LOG_DIR:-0:0:755}"
    ;;
  "$CASE_ROOT/var/log/unionc-agent.log")
    if grep -Fx 'user_created=1' "$CASE_ROOT/var/db/unionc-agent/account-ownership" >/dev/null 2>&1; then
      metadata="${STAT_LOG_FILE:-450:450:600}"
    else
      metadata="${STAT_LOG_FILE:-0:0:600}"
    fi
    ;;
  *) exit 65 ;;
esac
uid="${metadata%%:*}"
remainder="${metadata#*:}"
gid="${remainder%%:*}"
mode="${remainder#*:}"
printf '%s:%s:%s:%s\n' "$uid" "$gid" "${STAT_SPECIAL_MODE:-0}" "$mode"
EOF

  cat >"$case_bin/plutil" <<'EOF'
#!/bin/sh
[ "${1:-}" = -lint ] && [ "$#" -eq 2 ] || exit 64
[ -f "$2" ] && grep -F '<plist ' "$2" >/dev/null
EOF

  cat >"$case_bin/chown" <<'EOF'
#!/bin/sh
count=0
if [ -f "$FILE_STATE/chown-count" ]; then
  IFS= read -r count <"$FILE_STATE/chown-count"
fi
count=$((count + 1))
printf '%s\n' "$count" >"$FILE_STATE/chown-count"
printf '%s\n' "$*" >>"$FILE_STATE/chown-calls"
if [ -n "${FAIL_CHOWN_AT:-}" ] && [ "$count" -eq "$FAIL_CHOWN_AT" ]; then
  exit 71
fi
exit 0
EOF

  cat >"$case_bin/pkgutil" <<'EOF'
#!/bin/sh
if [ "${PKGUTIL_INSTALLED:-0}" -eq 1 ]; then
  printf 'package-id: com.unionc.agent\n'
  printf 'version: @UNIONC_AGENT_PACKAGE_VERSION@\n'
  exit 0
fi
exit 1
EOF

  cat >"$case_bin/dscl" <<'EOF'
#!/bin/sh
set -eu

next_count() {
  counter="$1"
  count=0
  if [ -f "$counter" ]; then
    IFS= read -r count <"$counter"
  fi
  count=$((count + 1))
  printf '%s\n' "$count" >"$counter"
  printf '%s\n' "$count"
}

[ "${1:-}" = . ] || exit 64
operation="${2:-}"
case "$operation" in
  -list)
    record_type="$3"
    field="${4:-}"
    directory="$DSCL_STATE$record_type"
    for record_directory in "$directory"/*; do
      [ -d "$record_directory" ] || continue
      record_name="${record_directory##*/}"
      if [ -n "$field" ]; then
        [ -f "$record_directory/$field" ] || continue
        value="$(sed -n '1p' "$record_directory/$field")"
        printf '%s %s\n' "$record_name" "$value"
      else
        printf '%s\n' "$record_name"
      fi
    done
    ;;
  -create)
    count="$(next_count "$DSCL_STATE/create-count")"
    if [ -n "${FAIL_DSCL_CREATE_AT:-}" ] && [ "$count" -eq "$FAIL_DSCL_CREATE_AT" ]; then
      exit 72
    fi
    record="$3"
    record_directory="$DSCL_STATE$record"
    mkdir -p "$record_directory"
    if [ -n "${4:-}" ]; then
      printf '%s\n' "$5" >"$record_directory/$4"
    fi
    ;;
  -read)
    count="$(next_count "$DSCL_STATE/read-count")"
    if [ -n "${FAIL_DSCL_READ_AT:-}" ] && [ "$count" -eq "$FAIL_DSCL_READ_AT" ]; then
      exit 73
    fi
    record="$3"
    record_directory="$DSCL_STATE$record"
    [ -d "$record_directory" ] || exit 1
    if [ -n "${4:-}" ]; then
      [ -f "$record_directory/$4" ] || exit 1
      printf '%s: %s\n' "$4" "$(sed -n '1p' "$record_directory/$4")"
    fi
    ;;
  -delete)
    record="$3"
    record_directory="$DSCL_STATE$record"
    rm -rf "$record_directory"
    ;;
  *) exit 64 ;;
esac
EOF

  cat >"$case_bin/launchctl" <<'EOF'
#!/bin/sh
set -eu
printf '%s\n' "$*" >>"$LAUNCH_STATE/calls"
case "${1:-}" in
  enable) ;;
  bootstrap)
    count=0
    if [ -f "$LAUNCH_STATE/bootstrap-count" ]; then
      IFS= read -r count <"$LAUNCH_STATE/bootstrap-count"
    fi
    count=$((count + 1))
    printf '%s\n' "$count" >"$LAUNCH_STATE/bootstrap-count"
    if [ -n "${FAIL_BOOTSTRAP_AT:-}" ] && [ "$count" -eq "$FAIL_BOOTSTRAP_AT" ]; then
      exit 74
    fi
    plist="$3"
    label="${plist##*/}"
    label="${label%.plist}"
    : >"$LAUNCH_STATE/loaded.$label"
    ;;
  bootout)
    count=0
    if [ -f "$LAUNCH_STATE/bootout-count" ]; then
      IFS= read -r count <"$LAUNCH_STATE/bootout-count"
    fi
    count=$((count + 1))
    printf '%s\n' "$count" >"$LAUNCH_STATE/bootout-count"
    if [ -n "${FAIL_BOOTOUT_AT:-}" ] && [ "$count" -eq "$FAIL_BOOTOUT_AT" ]; then
      exit 75
    fi
    target="$2"
    label="${target#system/}"
    rm -f "$LAUNCH_STATE/loaded.$label"
    ;;
  print)
    target="$2"
    label="${target#system/}"
    [ -f "$LAUNCH_STATE/loaded.$label" ]
    ;;
  *) exit 64 ;;
esac
EOF

  chmod +x "$case_bin/id" "$case_bin/install" "$case_bin/stat" "$case_bin/chown" \
    "$case_bin/dscl" "$case_bin/launchctl" "$case_bin/pkgutil" "$case_bin/plutil"
}

run_postinstall() {
  run_case="$1"
  shift
  env \
    DSCL_STATE="$run_case/dscl" \
    FILE_STATE="$run_case" \
    LAUNCH_STATE="$run_case/launch" \
    CASE_ROOT="$run_case" \
    "$@" \
    "$run_case/postinstall"
}

run_preinstall() {
  run_case="$1"
  shift
  env \
    FILE_STATE="$run_case" \
    LAUNCH_STATE="$run_case/launch" \
    CASE_ROOT="$run_case" \
    "$@" \
    "$run_case/preinstall"
}

reset_fault_counters() {
  reset_case="$1"
  rm -f "$reset_case/dscl/create-count" "$reset_case/dscl/read-count" \
    "$reset_case/chown-count" "$reset_case/chown-calls" \
    "$reset_case/launch/bootstrap-count" "$reset_case/launch/bootout-count"
}

assert_payload_rejected_without_bootout() {
  payload_case="$1"
  payload_case_name="$2"
  shift 2
  : >"$payload_case/launch/loaded.com.unionc.agent"
  : >"$payload_case/launch/loaded.com.unionc.agent.logrotate"
  : >"$payload_case/launch/calls"
  if run_postinstall "$payload_case" "$@" >"$payload_case/failure.log" 2>&1; then
    fail "$payload_case_name unexpectedly succeeded"
  fi
  [ -e "$payload_case/launch/loaded.com.unionc.agent" ] ||
    fail "$payload_case_name stopped the previously loaded Agent"
  [ -e "$payload_case/launch/loaded.com.unionc.agent.logrotate" ] ||
    fail "$payload_case_name stopped the previously loaded log-rotation helper"
  if grep -F 'bootout ' "$payload_case/launch/calls" >/dev/null 2>&1; then
    fail "$payload_case_name reached the launchd stop transaction"
  fi
  [ ! -d "$payload_case/dscl/Groups/_unioncagent" ] ||
    fail "$payload_case_name created a service group before rejecting the payload"
  [ ! -d "$payload_case/dscl/Users/_unioncagent" ] ||
    fail "$payload_case_name created a service user before rejecting the payload"
}

# Installer replaces files before postinstall. Every start-critical payload
# check must therefore fail while the old jobs remain loaded; rollback cannot
# recover the old files after they have already been replaced.
payload_failure=missing-agent
while [ -n "$payload_failure" ]; do
  case_dir="$test_root/payload-$payload_failure"
  make_case "$case_dir"
  payload_environment=
  case "$payload_failure" in
    missing-agent)
      rm "$case_dir/usr/local/libexec/unionc-agent"
      next_payload_failure=redirected-agent
      ;;
    redirected-agent)
      rm "$case_dir/usr/local/libexec/unionc-agent"
      ln -s /bin/true "$case_dir/usr/local/libexec/unionc-agent"
      next_payload_failure=agent-mode
      ;;
    agent-mode)
      payload_environment=STAT_AGENT_BINARY=0:0:700
      next_payload_failure=agent-version
      ;;
    agent-version)
      sed 's/unionc-agent @UNIONC_AGENT_PACKAGE_VERSION@/unionc-agent 0.0.0/' \
        "$case_dir/usr/local/libexec/unionc-agent" >"$case_dir/agent.invalid"
      mv "$case_dir/agent.invalid" "$case_dir/usr/local/libexec/unionc-agent"
      chmod 0755 "$case_dir/usr/local/libexec/unionc-agent"
      next_payload_failure=missing-helper
      ;;
    missing-helper)
      rm "$case_dir/usr/local/libexec/unionc-agent-logrotate"
      next_payload_failure=helper-syntax
      ;;
    helper-syntax)
      printf '#!/bin/sh\nif then\n' >"$case_dir/usr/local/libexec/unionc-agent-logrotate"
      chmod 0755 "$case_dir/usr/local/libexec/unionc-agent-logrotate"
      next_payload_failure=helper-mode
      ;;
    helper-mode)
      payload_environment=STAT_LOGROTATE_HELPER=501:20:755
      next_payload_failure=missing-agent-plist
      ;;
    missing-agent-plist)
      rm "$case_dir/Library/LaunchDaemons/com.unionc.agent.plist"
      next_payload_failure=invalid-agent-plist
      ;;
    invalid-agent-plist)
      printf 'not a plist\n' >"$case_dir/Library/LaunchDaemons/com.unionc.agent.plist"
      next_payload_failure=agent-plist-mode
      ;;
    agent-plist-mode)
      payload_environment=STAT_AGENT_PLIST=0:0:666
      next_payload_failure=missing-logrotate-plist
      ;;
    missing-logrotate-plist)
      rm "$case_dir/Library/LaunchDaemons/com.unionc.agent.logrotate.plist"
      next_payload_failure=invalid-logrotate-plist
      ;;
    invalid-logrotate-plist)
      printf 'not a plist\n' >"$case_dir/Library/LaunchDaemons/com.unionc.agent.logrotate.plist"
      next_payload_failure=command-not-link
      ;;
    command-not-link)
      rm "$case_dir/usr/local/bin/unionc-agent"
      : >"$case_dir/usr/local/bin/unionc-agent"
      next_payload_failure=command-target
      ;;
    command-target)
      rm "$case_dir/usr/local/bin/unionc-agent"
      ln -s ../libexec/not-unionc-agent "$case_dir/usr/local/bin/unionc-agent"
      next_payload_failure=command-mode
      ;;
    command-mode)
      payload_environment=STAT_AGENT_COMMAND=501:20:755
      next_payload_failure=
      ;;
    *) fail "unknown payload failure case: $payload_failure" ;;
  esac
  if [ -n "$payload_environment" ]; then
    assert_payload_rejected_without_bootout "$case_dir" "$payload_failure" \
      "$payload_environment"
  else
    assert_payload_rejected_without_bootout "$case_dir" "$payload_failure"
  fi
  payload_failure="$next_payload_failure"
done

assert_recoverable() {
  recovery_case="$1"
  reset_fault_counters "$recovery_case"
  run_postinstall "$recovery_case" >"$recovery_case/recovery.log" 2>&1 ||
    fail "a clean rerun did not recover $recovery_case"
  [ -d "$recovery_case/dscl/Groups/_unioncagent" ] || fail "group missing after recovery"
  [ -d "$recovery_case/dscl/Users/_unioncagent" ] || fail "user missing after recovery"
  grep -Fx 'group_created=1' "$recovery_case/var/db/unionc-agent/account-ownership" >/dev/null ||
    fail "group ownership was not committed after recovery"
  grep -Fx 'user_created=1' "$recovery_case/var/db/unionc-agent/account-ownership" >/dev/null ||
    fail "user ownership was not committed after recovery"
}

# Security-sensitive roots must be rejected before postinstall creates an
# account or runs any state/log chown as root.
case_dir="$test_root/unsafe-ownership-symlink"
make_case "$case_dir"
mkdir -p "$case_dir/foreign-ownership" "$(dirname -- "$case_dir/var/db/unionc-agent")"
: >"$case_dir/foreign-ownership/sentinel"
ln -s "$case_dir/foreign-ownership" "$case_dir/var/db/unionc-agent"
if run_postinstall "$case_dir" >"$case_dir/failure.log" 2>&1; then
  fail 'symlinked ownership directory unexpectedly succeeded'
fi
[ -e "$case_dir/foreign-ownership/sentinel" ] || fail 'ownership symlink target was modified'

case_dir="$test_root/unsafe-ownership-mode"
make_case "$case_dir"
mkdir -p "$case_dir/var/db/unionc-agent"
if run_postinstall "$case_dir" STAT_OWNERSHIP_DIR=501:20:777 \
  >"$case_dir/failure.log" 2>&1; then
  fail 'foreign ownership directory metadata unexpectedly succeeded'
fi

case_dir="$test_root/unsafe-state-symlink"
make_case "$case_dir"
mkdir -p "$case_dir/foreign-state"
: >"$case_dir/foreign-state/sentinel"
rm -rf "$case_dir/Library/Application Support/UnionC Agent"
ln -s "$case_dir/foreign-state" "$case_dir/Library/Application Support/UnionC Agent"
if run_postinstall "$case_dir" >"$case_dir/failure.log" 2>&1; then
  fail 'symlinked state directory unexpectedly succeeded'
fi
[ -e "$case_dir/foreign-state/sentinel" ] || fail 'state symlink target was modified'

case_dir="$test_root/unsafe-state-owner"
make_case "$case_dir"
if run_postinstall "$case_dir" STAT_STATE_ROOT=501:20:755 \
  >"$case_dir/failure.log" 2>&1; then
  fail 'foreign state ownership unexpectedly succeeded'
fi
[ ! -d "$case_dir/dscl/Groups/_unioncagent" ] ||
  fail 'foreign state ownership was rejected only after creating an account'
[ ! -s "$case_dir/chown-calls" ] || fail 'foreign state ownership reached chown'

case_dir="$test_root/unsafe-config-symlink"
make_case "$case_dir"
mv "$case_dir/Library/Application Support/UnionC Agent/config.example.json" \
  "$case_dir/foreign-config.json"
ln -s "$case_dir/foreign-config.json" \
  "$case_dir/Library/Application Support/UnionC Agent/config.example.json"
if run_postinstall "$case_dir" >"$case_dir/failure.log" 2>&1; then
  fail 'symlinked package config unexpectedly succeeded'
fi

case_dir="$test_root/stale-package-config"
make_case "$case_dir"
sed 's/@UNIONC_AGENT_PACKAGE_VERSION@/0.0.0/' \
  "$case_dir/Library/Application Support/UnionC Agent/config.example.json" \
  >"$case_dir/config.example.stale"
mv "$case_dir/config.example.stale" \
  "$case_dir/Library/Application Support/UnionC Agent/config.example.json"
if run_postinstall "$case_dir" >"$case_dir/failure.log" 2>&1; then
  fail 'stale package config unexpectedly succeeded'
fi

case_dir="$test_root/unsafe-log-dir"
make_case "$case_dir"
if run_postinstall "$case_dir" STAT_LOG_DIR=0:0:777 \
  >"$case_dir/failure.log" 2>&1; then
  fail 'world-writable log directory unexpectedly succeeded'
fi

case_dir="$test_root/unsafe-log-symlink"
make_case "$case_dir"
: >"$case_dir/foreign-log"
ln -s "$case_dir/foreign-log" "$case_dir/var/log/unionc-agent.log"
if run_postinstall "$case_dir" >"$case_dir/failure.log" 2>&1; then
  fail 'symlinked log file unexpectedly succeeded'
fi

# None of the root-validation failures above may have reached account creation.
for unsafe_case in "$test_root"/unsafe-* "$test_root"/stale-package-config; do
  [ ! -d "$unsafe_case/dscl/Groups/_unioncagent" ] ||
    fail "unsafe root case created a service group: $unsafe_case"
  [ ! -d "$unsafe_case/dscl/Users/_unioncagent" ] ||
    fail "unsafe root case created a service user: $unsafe_case"
done

# Four group attributes followed by eight user attributes. Every failed dscl
# creation must remove only the incomplete record and leave a clean rerun path.
failure=1
while [ "$failure" -le 12 ]; do
  case_dir="$test_root/create-$failure"
  make_case "$case_dir"
  if run_postinstall "$case_dir" FAIL_DSCL_CREATE_AT="$failure" \
    >"$case_dir/failure.log" 2>&1; then
    fail "dscl create failure $failure unexpectedly succeeded"
  fi
  [ ! -d "$case_dir/dscl/Users/_unioncagent" ] ||
    fail "dscl create failure $failure left a partial user"
  if [ "$failure" -le 4 ]; then
    [ ! -d "$case_dir/dscl/Groups/_unioncagent" ] ||
      fail "dscl create failure $failure left a partial group"
    [ ! -e "$case_dir/var/db/unionc-agent/account-ownership" ] ||
      fail "dscl create failure $failure left an ownership claim"
  else
    [ -d "$case_dir/dscl/Groups/_unioncagent" ] ||
      fail "user creation failure $failure removed the committed group"
    grep -Fx 'user_created=0' "$case_dir/var/db/unionc-agent/account-ownership" >/dev/null ||
      fail "user creation failure $failure corrupted the group-only marker"
  fi
  assert_recoverable "$case_dir"
  failure=$((failure + 1))
done

# Verification is part of the same transaction as record construction.
failure=1
while [ "$failure" -le 3 ]; do
  case_dir="$test_root/read-$failure"
  make_case "$case_dir"
  if run_postinstall "$case_dir" FAIL_DSCL_READ_AT="$failure" \
    >"$case_dir/failure.log" 2>&1; then
    fail "dscl verification failure $failure unexpectedly succeeded"
  fi
  [ ! -d "$case_dir/dscl/Users/_unioncagent" ] ||
    fail "dscl verification failure $failure left a partial user"
  if [ "$failure" -eq 1 ]; then
    [ ! -d "$case_dir/dscl/Groups/_unioncagent" ] ||
      fail "group verification failure left a partial group"
  else
    [ -d "$case_dir/dscl/Groups/_unioncagent" ] ||
      fail "user verification failure removed the committed group"
  fi
  assert_recoverable "$case_dir"
  failure=$((failure + 1))
done

# Marker publication failures used to strand an unowned same-name record.
failure=1
while [ "$failure" -le 2 ]; do
  case_dir="$test_root/marker-$failure"
  make_case "$case_dir"
  if run_postinstall "$case_dir" FAIL_CHOWN_AT="$failure" \
    >"$case_dir/failure.log" 2>&1; then
    fail "marker publication failure $failure unexpectedly succeeded"
  fi
  [ ! -d "$case_dir/dscl/Users/_unioncagent" ] ||
    fail "marker publication failure $failure left a partial user"
  if [ "$failure" -eq 1 ]; then
    [ ! -d "$case_dir/dscl/Groups/_unioncagent" ] ||
      fail "group marker failure left a partial group"
  else
    [ -d "$case_dir/dscl/Groups/_unioncagent" ] ||
      fail "user marker failure removed the committed group"
  fi
  assert_recoverable "$case_dir"
  failure=$((failure + 1))
done

# If the helper cannot be registered, the Agent registered immediately before
# it must not survive a failed package transaction.
case_dir="$test_root/bootstrap-2"
make_case "$case_dir"
if run_postinstall "$case_dir" FAIL_BOOTSTRAP_AT=2 >"$case_dir/failure.log" 2>&1; then
  fail "second launchctl bootstrap failure unexpectedly succeeded"
fi
[ ! -e "$case_dir/launch/loaded.com.unionc.agent" ] ||
  fail "Agent remained loaded after helper bootstrap failed"
[ ! -e "$case_dir/launch/loaded.com.unionc.agent.logrotate" ] ||
  fail "failed helper bootstrap left a loaded helper"
grep -Fx 'bootout system/com.unionc.agent' "$case_dir/launch/calls" >/dev/null ||
  fail "postinstall did not boot out the Agent after helper bootstrap failed"
[ -d "$case_dir/dscl/Groups/_unioncagent" ] && [ -d "$case_dir/dscl/Users/_unioncagent" ] ||
  fail "a launchd failure removed fully committed service accounts"
assert_recoverable "$case_dir"

# preinstall deliberately leaves the previous jobs running. postinstall stops
# them only after all validation succeeds; a later bootstrap failure removes
# half-registered jobs and re-registers the validated replacement payload for
# the labels that had been loaded before installation.
case_dir="$test_root/reinstall-postinstall-failure"
make_case "$case_dir"
run_postinstall "$case_dir" >"$case_dir/initial.log" 2>&1 ||
  fail 'could not establish the initial loaded launchd jobs'
reset_fault_counters "$case_dir"
: >"$case_dir/launch/calls"
run_preinstall "$case_dir" PKGUTIL_INSTALLED=1 >"$case_dir/preinstall.log" 2>&1 ||
  fail 'same-version preinstall unexpectedly failed'
[ -e "$case_dir/launch/loaded.com.unionc.agent" ] ||
  fail 'preinstall stopped the previous Agent before payload validation'
[ -e "$case_dir/launch/loaded.com.unionc.agent.logrotate" ] ||
  fail 'preinstall stopped the previous helper before payload validation'
if grep -F 'bootout ' "$case_dir/launch/calls" >/dev/null; then
  fail 'preinstall issued a launchd bootout'
fi

# A replacement that fails payload validation must likewise leave both old
# jobs untouched because postinstall has not reached its stop transaction.
sed 's/@UNIONC_AGENT_PACKAGE_VERSION@/0.0.0/' \
  "$case_dir/Library/Application Support/UnionC Agent/config.example.json" \
  >"$case_dir/config.example.invalid"
mv "$case_dir/config.example.invalid" \
  "$case_dir/Library/Application Support/UnionC Agent/config.example.json"
if run_postinstall "$case_dir" >"$case_dir/validation-failure.log" 2>&1; then
  fail 'replacement with an invalid package config unexpectedly succeeded'
fi
[ -e "$case_dir/launch/loaded.com.unionc.agent" ] ||
  fail 'payload validation failure stopped the previous Agent'
[ -e "$case_dir/launch/loaded.com.unionc.agent.logrotate" ] ||
  fail 'payload validation failure stopped the previous helper'
if grep -F 'bootout ' "$case_dir/launch/calls" >/dev/null; then
  fail 'payload validation failure reached the launchd stop transaction'
fi
sed 's/0.0.0/@UNIONC_AGENT_PACKAGE_VERSION@/' \
  "$case_dir/Library/Application Support/UnionC Agent/config.example.json" \
  >"$case_dir/config.example.restored"
mv "$case_dir/config.example.restored" \
  "$case_dir/Library/Application Support/UnionC Agent/config.example.json"

reset_fault_counters "$case_dir"
: >"$case_dir/launch/calls"
if run_postinstall "$case_dir" FAIL_BOOTSTRAP_AT=2 \
  >"$case_dir/postinstall-failure.log" 2>&1; then
  fail 'replacement helper bootstrap failure unexpectedly succeeded'
fi
[ -e "$case_dir/launch/loaded.com.unionc.agent" ] ||
  fail 'failed replacement did not re-register the Agent label'
[ -e "$case_dir/launch/loaded.com.unionc.agent.logrotate" ] ||
  fail 'failed replacement did not re-register the helper label'

# The stop phase inside postinstall is also failure-atomic: if stopping the
# second job fails, the validated helper stopped first is re-registered.
case_dir="$test_root/postinstall-bootout-failure"
make_case "$case_dir"
run_postinstall "$case_dir" >"$case_dir/initial.log" 2>&1 ||
  fail 'could not establish jobs for the postinstall bootout rollback case'
reset_fault_counters "$case_dir"
if run_postinstall "$case_dir" FAIL_BOOTOUT_AT=2 \
  >"$case_dir/postinstall-failure.log" 2>&1; then
  fail 'second postinstall bootout failure unexpectedly succeeded'
fi
[ -e "$case_dir/launch/loaded.com.unionc.agent" ] ||
  fail 'postinstall bootout failure lost the previous Agent job'
[ -e "$case_dir/launch/loaded.com.unionc.agent.logrotate" ] ||
  fail 'postinstall bootout failure did not re-register the helper job'

echo 'macOS postinstall failure-atomicity tests: ok'
