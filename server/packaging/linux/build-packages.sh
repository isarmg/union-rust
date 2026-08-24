#!/bin/sh
set -eu

die() {
  echo "unionc server package build: $*" >&2
  exit 1
}

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repository_root=$(CDPATH= cd -- "$script_dir/../../.." && pwd)
cd "$repository_root"

package_id=$(cargo pkgid --locked -p unionc) || die "cannot resolve the Server Cargo package"
case "$package_id" in
  *@*) package_version=${package_id##*@} ;;
  *) die "unexpected cargo pkgid output: $package_id" ;;
esac
case "$package_version" in
  *[!0-9.]*|*.*.*.*|.*|*.) die "Server version is not strict MAJOR.MINOR.PATCH: $package_version" ;;
esac
[ "$(printf '%s' "$package_version" | awk -F. 'NF == 3 && $1 != "" && $2 != "" && $3 != "" { print "yes" }')" = yes ] ||
  die "Server version is not strict MAJOR.MINOR.PATCH: $package_version"

# Lifecycle markers deliberately reject every other package version. Keep
# their literal version bound to Cargo so a future edit cannot produce a
# package that builds successfully and then refuses to install itself.
for lifecycle_script in \
  server/packaging/linux/postinstall.sh \
  server/packaging/linux/preremove.sh
do
  lifecycle_version=$(sed -n 's/^package_version=\([0-9][0-9.]*\)$/\1/p' "$lifecycle_script")
  [ "$lifecycle_version" = "$package_version" ] ||
    die "$lifecycle_script package_version does not match Cargo $package_version"
done
package_config_version=$(
  sed -n 's/^UNIONC_PACKAGE_VERSION=\([0-9][0-9.]*\)$/\1/p' \
    server/packaging/linux/unionc.env.example
)
[ "$package_config_version" = "$package_version" ] ||
  die "unionc.env.example package marker does not match Cargo $package_version"

server_binary=target/release/unionc
[ -x "$server_binary" ] || die "Server binary is missing or not executable: $server_binary"
package_arch=${NFPM_ARCH:-amd64}
case "$package_arch" in
  amd64)
    expected_elf_machine='Advanced Micro Devices X86-64'
    rpm_arch=x86_64
    ;;
  arm64)
    expected_elf_machine=AArch64
    rpm_arch=aarch64
    ;;
  *) die "unsupported Server Linux package architecture: $package_arch" ;;
esac
command -v readelf >/dev/null 2>&1 ||
  die "required binary inspection command is unavailable: readelf"
elf_machine=$(
  LC_ALL=C readelf -h -- "$server_binary" 2>/dev/null |
    awk -F: '
      /^[[:space:]]*Machine:/ {
        value = $2
        sub(/^[[:space:]]+/, "", value)
        sub(/[[:space:]]+$/, "", value)
        found += 1
        machine = value
      }
      END {
        if (found != 1 || machine == "") exit 1
        print machine
      }
    '
) || die "Server package payload is not a readable ELF binary: $server_binary"
[ "$elf_machine" = "$expected_elf_machine" ] ||
  die "Server package payload architecture $elf_machine does not match $package_arch"
LC_ALL=C readelf -p .unionc.version -- "$server_binary" 2>/dev/null |
  awk -v expected="unionc $package_version" '
    /^[[:space:]]*\[[[:space:]]*[[:xdigit:]]+\][[:space:]]+/ {
      value = $0
      sub(/^[[:space:]]*\[[[:space:]]*[[:xdigit:]]+\][[:space:]]+/, "", value)
      sub(/[[:space:]]+$/, "", value)
      found += 1
      if (value == expected) matched += 1
    }
    END { exit found == 1 && matched == 1 ? 0 : 1 }
  ' || die "Server package payload ELF version marker does not match Cargo $package_version"

nfpm_bin=${NFPM_BIN:-nfpm}
command -v "$nfpm_bin" >/dev/null 2>&1 || [ -x "$nfpm_bin" ] ||
  die "nFPM is unavailable: $nfpm_bin"

mkdir -p dist
VERSION="$package_version" NFPM_ARCH="$package_arch" "$nfpm_bin" package \
  --config server/packaging/nfpm.yaml --packager deb \
  --target "dist/unionc_${package_version}_${package_arch}.deb"
VERSION="$package_version" NFPM_ARCH="$package_arch" "$nfpm_bin" package \
  --config server/packaging/nfpm.yaml --packager rpm \
  --target "dist/unionc-${package_version}.${rpm_arch}.rpm"
