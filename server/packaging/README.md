# Core deployment assets

UnionC Core is not packaged or released as a standalone DEB/RPM. The only supported release
artifact is the complete, inventory-verified distribution assembled by Union Builder schema v2.
That distribution owns `bin/unionc`, the Web Shell and every included module package under one
immutable release root.

Core and complete Server distributions support only Linux amd64 (`x86_64`) and Linux arm64
(`aarch64`). Formal releases publish one inventory-verified full archive for each architecture;
the embedded release manifest is rejected when its platform/architecture does not match Core.
Builder may stage a verified release for another machine, but install/rollback reject activation on
a different host target. GNU binaries are linked natively on Ubuntu 24.04 runners, whose glibc and
system ABI define the current compatibility baseline; older Linux distributions are not implied.

`linux/unionc.service` and `linux/unionc.env.example` are deployment templates for that complete
distribution. They must not be used to construct a `/usr/bin/unionc` package or to install Core
without its Builder inventory. host-m-agent packaging is maintained in the
[`host-monitoring`](https://github.com/isarmg/host-monitoring) repository because the Agent is a
physical remote-machine companion, not a Core business module.
