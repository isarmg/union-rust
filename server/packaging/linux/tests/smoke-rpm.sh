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
    package="$(find /artifacts -maxdepth 1 -name "unionc-*.x86_64.rpm" -print -quit)"
    test -n "$package"

    rpm -qp --scripts "$package" >/tmp/unionc-current-rpm-scripts
    grep -F "systemctl restart unionc.service" /tmp/unionc-current-rpm-scripts

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
    package_version="$(rpm -q --qf "%{VERSION}" unionc)"
    grep -Fx "UNIONC_PACKAGE_VERSION=$package_version" /etc/unionc/unionc.env
    test "$(sed -n "s/^format=//p" /var/lib/unionc-package/managed-user)" = \
      "$package_version"
    test "$(sed -n "s/^format=//p" /var/lib/unionc-package/managed-group)" = \
      "$package_version"

    # Start the binary directly because the Fedora container does
    # not boot systemd; this validates SQLite creation and runtime.
    setpriv --reuid=unionc --regid=unionc --init-groups \
      env UNIONC_ENV=production UNIONC_DATA_DIR=/var/lib/unionc \
      UNIONC_SECRET_KEY=MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY= \
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

    dnf remove -y unionc
    test ! -e /usr/bin/unionc
    test -e /var/lib/unionc/unionc.db
  '

