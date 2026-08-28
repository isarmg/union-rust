# Core deployment assets

UnionC Core is not packaged or released as a standalone DEB/RPM. The only supported release
artifact is the complete, inventory-verified distribution assembled by Union Builder schema v2.
That distribution owns `bin/unionc`, the Web Shell and every included module package under one
immutable release root.

`linux/unionc.service` and `linux/unionc.env.example` are deployment templates for that complete
distribution. They must not be used to construct a `/usr/bin/unionc` package or to install Core
without its Builder inventory. Host Agent packaging remains separate because the Agent is a
physical remote-machine companion, not a Core business module.
