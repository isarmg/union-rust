# UnionC Agent Linux portable bundle

The Linux `.tar.gz` artifact is a **portable binary bundle**, not a managed
installer. Extracting it does not create the `unionc-agent` service account,
install the hardened systemd unit, establish filesystem permissions, preserve
configuration across reinstalls, or provide package-manager uninstall/purge
semantics.

For a managed installation, use the DEB or RPM built from
`agent/packaging/nfpm.yaml`. Those packages provide:

- a dedicated non-login account and private state directory;
- a hardened, enabled systemd service;
- same-version reinstall-safe configuration and credentials;
- ordinary uninstall that retains identity for a 0.5.0 reinstall;
- explicit local purge through `apt purge unionc-agent` (DEB) or
  `sudo unionc-agent-purge --yes` before `dnf remove unionc-agent` (RPM).

If you deliberately use the portable binary, you own its service definition,
configuration and state paths, permissions, replacement, and removal. Never clone
an already-paired state directory to another machine. Removing local files does
not revoke the server-side instance; revoke it in the UnionC Web console first.
