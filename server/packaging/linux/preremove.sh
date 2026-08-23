#!/bin/sh
set -eu

package_version=0.3.4

die() {
  echo "unionc preremove: $*" >&2
  exit 1
}

case "${1:-}" in
  upgrade)
    [ "$#" -eq 2 ] && [ "$2" = "$package_version" ] ||
      die "cross-version replacement is unsupported; remove the installed package before installing another version"
    if [ -d /run/systemd/system ]; then
      # Debian passes the incoming version while reinstalling this exact package.
      # Stop the running process before replacement; postinstall restarts it
      # only when its enabled state was preserved.
      systemctl --quiet stop unionc.service >/dev/null 2>&1 || true
    fi
    ;;
  0|''|remove|deconfigure|*[!0-9]*)
    if [ -d /run/systemd/system ]; then
      systemctl --quiet disable --now unionc.service >/dev/null 2>&1 || true
    fi
    ;;
  *)
    # RPM exposes only the positive remaining-instance count here, not the
    # incoming package version. This hook therefore cannot distinguish a
    # same-version reinstall; startup still rejects non-0.3.4 config/database
    # state before serving requests.
    :
    ;;
esac

# 有意不删除 /var/lib/unionc：其中含 SQLite 数据库、管理员配置和开发环境主密钥。
# 生产主密钥仍在 /etc/unionc/unionc.env；清理任一侧都可能让加密数据永久不可读。
