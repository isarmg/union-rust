#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repository_root=$(cd -- "$script_dir/../../../.." && pwd)
artifact_dir=${1:-"$repository_root/dist"}
[[ -d $artifact_dir ]]
artifact_dir=$(cd -- "$artifact_dir" && pwd)

docker run --rm \
  --volume "$artifact_dir:/artifacts:ro" \
  fedora:44 /bin/bash -euxo pipefail -c '
    packages=()
    while IFS= read -r -d "" candidate; do
      packages+=("$candidate")
    done < <(find /artifacts -maxdepth 1 -type f \
      -name "unionc-*.x86_64.rpm" ! -name "unionc-agent-*" -print0)
    if (( ${#packages[@]} != 1 )); then
      echo "error: expected exactly one unionc x86_64 RPM in /artifacts, found ${#packages[@]}" >&2
      if (( ${#packages[@]} > 0 )); then
        printf "  %s\n" "${packages[@]}" >&2
      fi
      exit 1
    fi
    package=${packages[0]}

    rpm -qp --scripts "$package" >/tmp/unionc-current-rpm-scripts
    grep -Fx "service_name=unionc.service" /tmp/unionc-current-rpm-scripts
    grep -F "systemctl restart \"\$service_name\"" /tmp/unionc-current-rpm-scripts

    dnf install -y curl util-linux "$package"
    test "$(rpm -q --qf "%{VERSION}" unionc)" = \
      "$(rpm -qp --qf "%{VERSION}" "$package")"
    rpm -q --requires unionc | grep -Fx shadow-utils
    rpm -q --requires unionc | grep -Fx systemd
    ! rpm -q --requires unionc | grep -Fx adduser
    test -x /usr/bin/unionc
    getent passwd unionc >/dev/null
    test "$(stat -c "%a:%U:%G" /var/lib/unionc)" = "700:unionc:unionc"
    test "$(stat -c "%a:%U:%G" /var/lib/unionc-package)" = "700:root:root"
    test "$(stat -c "%a:%U:%G" /etc/unionc)" = "755:root:root"
    test "$(stat -c "%a:%U:%G:%h" /etc/unionc/unionc.env)" = \
      "640:root:unionc:1"
    package_version="$(rpm -q --qf "%{VERSION}" unionc)"
    grep -Fx "UNIONC_PACKAGE_VERSION=$package_version" /etc/unionc/unionc.env
    test "$(sed -n "s/^format=//p" /var/lib/unionc-package/managed-user)" = \
      "$package_version"
    test "$(sed -n "s/^format=//p" /var/lib/unionc-package/managed-group)" = \
      "$package_version"
    test "$(stat -c "%a:%U:%G:%h" /var/lib/unionc-package/managed-user)" = \
      "600:root:root:1"
    test "$(stat -c "%a:%U:%G:%h" /var/lib/unionc-package/managed-group)" = \
      "600:root:root:1"
    test ! -e /var/lib/unionc-package/pending-group
    test ! -e /var/lib/unionc-package/pending-user
    test ! -e /var/lib/unionc/.unionc-data-directory

    # Start the binary directly because the Fedora container does
    # not boot systemd; this validates SQLite creation and runtime.
    setpriv --reuid=unionc --regid=unionc --init-groups \
      env UNIONC_ENV=production UNIONC_DATA_DIR=/var/lib/unionc \
      UNIONC_SECRET_KEY=MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY= \
      UNIONC_PROXY_SECRET=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef \
      UNIONC_ALLOW_BOOTSTRAP=1 \
      UNIONC_BOOTSTRAP_PASSWORD=release-smoke-password-2026 \
      /usr/bin/unionc >/tmp/unionc-server.log 2>&1 &
    server_pid=$!
    cleanup() {
      kill -TERM "$server_pid" >/dev/null 2>&1 || true
      wait "$server_pid" >/dev/null 2>&1 || true
    }
    trap cleanup EXIT
    for _ in $(seq 1 60); do
      if curl --fail --silent http://127.0.0.1:8081/api/ready >/dev/null; then
        break
      fi
      sleep 1
    done
    curl --fail --silent --show-error \
      http://127.0.0.1:8081/api/ready >/dev/null
    test "$(stat -c "%a:%U:%G" /var/lib/unionc/unionc.db)" = "600:unionc:unionc"
    data_marker=/var/lib/unionc/.unionc-data-directory
    test "$(stat -c "%a:%U:%G:%h" "$data_marker")" = "600:unionc:unionc:1"
    test "$(cat "$data_marker")" = "unionc-data-directory-v1"
    data_marker_identity=$(stat -c "%d:%i" "$data_marker")
    cleanup
    trap - EXIT

    setpriv --reuid=unionc --regid=unionc --init-groups \
      env UNIONC_ENV=production UNIONC_DATA_DIR=/var/lib/unionc \
      UNIONC_SECRET_KEY=MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY= \
      /usr/bin/unionc integrity-check

    dnf reinstall -y "$package"
    test "$(rpm -q --qf "%{VERSION}" unionc)" = \
      "$(rpm -qp --qf "%{VERSION}" "$package")"
    setpriv --reuid=unionc --regid=unionc --init-groups \
      env UNIONC_ENV=production UNIONC_DATA_DIR=/var/lib/unionc \
      UNIONC_SECRET_KEY=MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY= \
      /usr/bin/unionc integrity-check
    test "$(stat -c "%d:%i" "$data_marker")" = "$data_marker_identity"

    dnf remove -y unionc
    test ! -e /usr/bin/unionc
    test -e /var/lib/unionc/unionc.db
    test "$(stat -c "%d:%i" "$data_marker")" = "$data_marker_identity"
  '
