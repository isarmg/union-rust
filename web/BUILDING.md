# Union Web compile-time modules

The server catalog is the only runtime source of installed modules. The web build has two
matching switches for its Union-console contributions:

| Environment variable | Default | Controls |
| --- | --- | --- |
| `UNIONC_WEB_MODULE_SUNSHINE` | `true` | Sunshine console, API-log view and related JS/CSS chunks |
| `UNIONC_WEB_MODULE_HOST_MONITORING` | `true` | Host-monitoring console, Agent activation page and related JS/CSS chunks |

Accepted boolean values are `1/0`, `true/false`, `yes/no`, and `on/off` (case-insensitive).
Defaults keep local development convenient. A release builder must pass values that match the
Rust feature selection. For example, a core-only console is built with:

```sh
UNIONC_WEB_MODULE_SUNSHINE=false \
UNIONC_WEB_MODULE_HOST_MONITORING=false \
npm run build
```

Vite replaces the switches with boolean literals before Rollup tree-shaking. Disabled modules do
not produce their lazy JavaScript or feature CSS chunks. Gateway-owned applications do not need a
web build switch: Union renders them only when their descriptor is present and reports
`health: "available"`, and computes their same-origin link from `service.gateway_prefix` plus
`ui.entry_path`.
