#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
packaging_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)
test_root=$(mktemp -d "${TMPDIR:-/tmp}/unionc-server-packaging-test.XXXXXX")
package_version=0.3.2
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

# Relocate every package-owned path into a disposable tree. The rewritten PATH
# keeps the test doubles authoritative while preserving the production script's
# rule that a caller-controlled PATH is never trusted.
sed \
  -e 's#/var/lib/unionc-package#__PACKAGE_STATE__#g' \
  -e 's#/var/lib/unionc#__SERVER_STATE__#g' \
  -e 's#/etc/unionc/unionc.env#__PACKAGE_CONFIG__#g' \
  -e 's#/etc/unionc#__CONFIG_DIR__#g' \
  -e 's#/run/systemd/system#__SYSTEMD_RUNTIME__#g' \
  -e 's#/usr/bin/unionc#__SERVER_BINARY__#g' \
  -e 's#^PATH=/usr/sbin:/usr/bin:/sbin:/bin$#PATH=__TRUSTED_BIN__:/usr/sbin:/usr/bin:/sbin:/bin#' \
  -e "s#__PACKAGE_STATE__#$test_root/var/lib/unionc-package#g" \
  -e "s#__SERVER_STATE__#$test_root/var/lib/unionc#g" \
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

cat >"$test_root/trusted-bin/unionc" <<EOF
#!/bin/sh
: >"$test_root/trusted-server-ran"
printf 'unionc %s\n' '$package_version'
EOF

cat >"$test_root/trusted-bin/getent" <<'EOF'
#!/bin/sh
case "${1:-}:${2:-}" in
  group:unionc) printf 'unionc:x:998:\n' ;;
  passwd:unionc)
    printf 'unionc:x:998:998::%s/var/lib/unionc:/usr/sbin/nologin\n' "$test_root"
    ;;
  *) exit 2 ;;
esac
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
  if [ "$owner:$destination" = "root:998:$test_root/etc/unionc/unionc.env" ]; then
    printf '998\n' >"$test_root/config-chowned-gid"
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
  "$test_root/var/lib/unionc")
    metadata=998:998:700
    nlink=2
    ;;
  "$test_root/etc/unionc")
    metadata=${STAT_CONFIG_DIR:-0:0:755}
    nlink=${STAT_CONFIG_DIR_NLINK:-2}
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
  %u:%g:%a) printf '%s:%s:%s\n' "$uid" "$gid" "$mode" ;;
  %u:%g:%a:%h) printf '%s:%s:%s:%s\n' "$uid" "$gid" "$mode" "$nlink" ;;
  *) exit 2 ;;
esac
EOF

for attacker_command in unionc getent groupadd useradd install cut chown chmod mv stat awk; do
  cat >"$test_root/attacker-bin/$attacker_command" <<'EOF'
#!/bin/sh
: >"$test_root/attacker-command-ran"
exit 99
EOF
done

chmod 0755 "$test_root/trusted-bin/"* "$test_root/attacker-bin/"*
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

# Fresh package extraction owns the config as root:root. The same checks must
# accept it and verify the transition to the recorded service group.
rm -f "$test_root/config-chowned-gid"
MODEL_CONFIG_CHOWN=1 STAT_CONFIG_FILE=0:0:640 "$test_root/postinstall.sh" \
  >"$test_root/fresh-config.log" 2>&1 ||
  fail 'postinstall rejected a fresh root-owned Server config'

echo 'Server Linux lifecycle checks passed'
