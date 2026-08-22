# macOS Agent package lifecycle

The macOS package installs the Agent as a system LaunchDaemon. Installation and browser
authorization are deliberately separate: the pkg never embeds a pre-shared credential or a
long-lived Agent credential.

## Build

Run from any directory on macOS. `VERSION` must be a numeric Installer package version and
`BINARY` must point to an executable Mach-O:

```sh
BINARY=/path/to/unionc-agent VERSION=0.3.2 ./agent/packaging/macos/build-pkg.sh
```

This produces an **unsigned development installer** by default. The build wraps the component
package in a `productbuild` distribution so Installer.app can show the pairing steps after the
files have been installed. For a distribution build, first sign the Agent executable with a
Developer ID Application identity. Then pass the exact Developer ID Installer identity name:

```sh
BINARY=/path/to/signed/unionc-agent \
VERSION=0.3.2 \
OUTPUT=/path/to/unionc-agent-0.3.2.pkg \
INSTALLER_IDENTITY='Developer ID Installer: Example Company (TEAMID)' \
./agent/packaging/macos/build-pkg.sh
```

The build script verifies the payload code signature, signs the pkg, and checks the resulting
Installer signature. It intentionally does **not** upload for notarization or staple a ticket;
those remain separate release-pipeline steps requiring Apple credentials.

Account deletion and Directory Service failure behavior can be checked without changing local
accounts:

```sh
./agent/packaging/macos/tests/validate-packaging.sh
```

生成 pkg 后，可在一次性 macOS 测试机中执行 Release 使用的真实安装生命周期测试。该命令
会安装、卸载并永久清理 Agent，必须显式确认系统变更：

```bash
./agent/packaging/macos/tests/smoke-pkg.sh --allow-system-changes dist/unionc-agent-0.3.2.pkg
```

## Install and pair

```sh
sudo installer -pkg unionc-agent-0.3.2.pkg -target /
sudo -u _unioncagent /usr/local/bin/unionc-agent pair \
  --config '/Library/Application Support/UnionC Agent/config.json' \
  --server https://unionc.example.com
```

Replace the URL with the management-console origin. Running `pair` as `_unioncagent` is
important: the LaunchDaemon uses that account and the state directory is private to it.
Installer.app shows the same pairing reminder on its conclusion screen; installation success
only means that the LaunchDaemon started, not that the Agent is authorized to deliver reports.

Use the service account for local Agent diagnostics and the system service manager for process
and log diagnostics:

```sh
sudo -u _unioncagent /usr/local/bin/unionc-agent status --output human \
  --config '/Library/Application Support/UnionC Agent/config.json'
sudo -u _unioncagent /usr/local/bin/unionc-agent doctor --output human \
  --config '/Library/Application Support/UnionC Agent/config.json'
sudo launchctl print system/com.unionc.agent
sudo tail -n 100 /var/log/unionc-agent.log
```

`doctor` is read-only by default. Use `doctor --delivery` only when an explicit end-to-end
delivery attempt (including processing queued reports) is intended.

Reinstalling the current 0.3.2 package stops the Agent in `preinstall`, replaces the payload,
preserves configuration/identity/spool, and starts the daemon in `postinstall`. A different
package version is not migrated: purge 0.3.2 state before installing another version.

The package also installs an hourly size check. At 10 MiB it briefly stops the Agent, rotates
the log with macOS `newsyslog`, retains seven compressed archives, and restarts the same
LaunchDaemon. Stopping before rotation prevents a long-running process from continuing to
write through an old file descriptor.

## Uninstall versus purge

Normal uninstall removes the executable and both LaunchDaemons but intentionally preserves
identity and diagnostic state for a safe reinstall:

```sh
sudo /usr/local/share/unionc-agent/uninstall.sh
```

For permanent decommissioning, **first revoke the instance in the UnionC Web console**. Then
delete local credentials, pairing state, spool, configuration, logs, receipt, and the dedicated
account/group created by this package:

```sh
sudo /usr/local/share/unionc-agent/uninstall.sh --purge
```

Managed non-interactive removal may add `--yes`. Purge only deletes `_unioncagent` records for
which a root-only ownership marker proves this installer created them. The marker records the
creation-time UID and GIDs, and purge requires exact identity and account-attribute matches; a
coincidentally pre-existing or later reconstructed account is retained. A clean install refuses
to adopt same-named accounts that are not bound by the current ownership marker.

If a marker-owned account was modified, or its group still has primary/supplementary members or
nested-group references, purge returns exit status `2` and reports `incomplete`. Directory
Service query failures have the same fail-closed result. Program/state/log removal has still
occurred, but the ownership marker, package receipt, and uninstall helper remain so an
administrator can repair the account conflict and safely retry.
