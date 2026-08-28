# macOS Agent package lifecycle

The macOS package installs the Agent as a system LaunchDaemon. Installation and browser
authorization are deliberately separate: the pkg never embeds a pre-shared credential or a
long-lived Agent credential.

## Build

Run from any directory on macOS. `VERSION` must be a numeric Installer package version and
`BINARY` must point to an executable Mach-O:

```sh
BINARY=/path/to/unionc-agent VERSION=0.5.0 ./agent/packaging/macos/build-pkg.sh
```

This produces an **unsigned development installer** by default. The build wraps the component
package in a `productbuild` distribution so Installer.app can show the pairing steps after the
files have been installed. For a distribution build, first sign the Agent executable with a
Developer ID Application identity. Then pass the exact Developer ID Installer identity name:

```sh
BINARY=/path/to/signed/unionc-agent \
VERSION=0.5.0 \
OUTPUT=/path/to/unionc-agent-0.5.0.pkg \
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
./agent/packaging/macos/tests/smoke-pkg.sh --allow-system-changes dist/unionc-agent-0.5.0.pkg
```

## Install and pair

```sh
sudo installer -pkg unionc-agent-0.5.0.pkg -target /
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

Reinstalling the current 0.5.0 package leaves the old jobs running while Installer replaces only
the root-owned payload. The packaged template lives at
`/usr/local/share/unionc-agent/config.example.json`; the pkg payload never extracts into the
service-writable state directory. Before any `bootout`, `postinstall` verifies the immutable
payload, ownership proof and preliminary state metadata. It then stops both jobs, rejects any
remaining process with the service UID, temporarily locks the state directory to root, and repeats
all config/log type, owner, mode, ACL and version checks before making pathname-based changes.
Fresh config is published from a same-directory exclusive temporary file without replacing an
existing path. State is returned to the service UID only after final verification, and only then
are the validated jobs registered. A failure before cutover leaves the old processes running; a
failure afterward restarts prior jobs only when mutable state has been restored and revalidated.
Otherwise it deliberately leaves both launchd labels stopped and persistently disabled so a
reboot cannot load untrusted state; a successful rerun re-enables them. Rollback verifies both
the labels and effective/real service-UID processes after forced cleanup, and reports an explicit
incomplete-cleanup error if macOS refuses either operation. Reinstall preserves
configuration/identity/spool.
A different package version is not migrated: purge 0.5.0 state before installing another version.

Because launchd executes payloads below `/usr/local`, `preinstall` checks each listed component
of the root payload path chain, including `/usr` and `/Library`, before Installer extracts the
package. The shared `/usr/local`, `libexec`, `bin`, and `share` directories must be real
`root:wheel` directories,
root-writable and service-traversable, but not group- or other-writable. The package-private share,
LaunchDaemon and log directories use their exact package modes. `/usr`, `/Library`, and the
root-owned `/Library/Application Support` directory may retain system deny-only ACLs; an ACL
containing any grant is rejected. The remaining listed directories, the command link, and
pre-existing root payload files may not have an extended ACL. `postinstall` repeats the checks on
the extracted payload before stopping a running Agent.

This fail-closed rule deliberately rejects a user-owned `/usr/local`, including the traditional
Intel Homebrew layout. Do not recursively change a working Homebrew tree merely to satisfy the
installer: inspect the host layout and use a separately secured installation target or host.

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
which the root-owned bookkeeping directory and ownership marker jointly prove this installer
created them. The directory must be a real `root:wheel` `0700` directory and the marker a real
`root:wheel` `0600` file; neither may have special permission bits or an extended ACL. The marker
records the creation-time UID and GIDs, and purge requires exact identity and account-attribute
matches; a coincidentally pre-existing or later reconstructed account is retained. A clean install
refuses to adopt same-named accounts that are not bound by the current ownership proof.
The cosmetic Directory Service `RealName` and `IsHidden` attributes are not used as identity
proof: macOS may omit them after accepting a write. Authorization remains bound to the record
name, exact numeric IDs, service shell/home attributes, and marker.

If a marker-owned account was modified, or its group still has primary/supplementary members or
nested-group references, purge returns exit status `2` and reports `incomplete`. Directory
Service or package-receipt database query failures, missing proof for a still-present account,
and any directory/marker metadata or ACL drift have the same fail-closed result.
Program/state/log removal may already have occurred; any still-needed safety bookkeeping, the
package receipt (when present), and the uninstall helper remain so an administrator can repair
the conflict and safely retry.
