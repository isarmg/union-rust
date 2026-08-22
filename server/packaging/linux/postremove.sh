#!/bin/sh
set -eu

# Package removal deliberately retains /var/lib/unionc, its SQLite data, and
# /var/lib/unionc-package ownership markers. The markers let this exact package
# reclaim the state while making a different version fail closed.
# Only refresh systemd's unit cache after files have been removed/replaced.
if [ -d /run/systemd/system ]; then
  if ! systemctl daemon-reload >/dev/null 2>&1; then
    echo "警告：UnionC 包操作完成，但 systemd daemon-reload 失败。" >&2
  fi
fi
