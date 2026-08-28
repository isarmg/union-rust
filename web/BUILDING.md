# Union Web Shell and runtime modules

The Web build is a stable Shell. It owns only authentication state, the base layout, core
navigation, permission filtering, routing, module loading and failure boundaries. Business
features are loaded from the Builder-bundled, enabled entries in `GET /api/platform/modules` at runtime; there are no
`UNIONC_WEB_MODULE_*` compile-time switches.

## Build the Shell

```sh
npm ci
npm run lint
npm run typecheck
npm test
npm run build
```

`npm run build` publishes the Shell atomically from `dist.next` to `dist`. Enabling or disabling a
module already bundled in the active distribution does not rebuild the Shell. Adding, removing or
upgrading module code requires Builder to assemble and verify a new immutable Union distribution;
runtime management never uploads or downloads code.

## Module frontend contract

Manifest v1 declares `frontend.entry`, `frontend.styles`, `frontend.components`,
`frontend.api_base`, `frontend.routes` and `frontend.menu`. All asset names are relative and Core
exposes them below `/modules/<module-id>/assets/`. API access is fixed below
`/api/modules/<module-id>`. The Shell rejects absolute URLs, parent traversal, cross-module routes,
undeclared components, undeclared permissions, identity mismatches and incompatible
semantic-version ranges before it renders module code.

The ESM entry has one default export:

```js
export default {
  pluginApiVersion: "1.0.0",
  moduleId: "example",
  version: "1.2.3",
  activate(hostSdk) {
    const { createElement, useState } = hostSdk.react;
    function ExampleView() {
      const [count, setCount] = useState(0);
      return createElement("button", { onClick: () => setCount(count + 1) }, String(count));
    }
    return { components: { ExampleView } };
  },
};
```

Remote entries must not import or bundle `react` or `react-dom`. `hostSdk.react` is the sole React
runtime, preventing duplicate-React hook failures and removing any dependency on hashed Shell
chunks. The activation result may only implement component names already whitelisted by the
Manifest; routes, menus and permissions cannot be added by executable code.

Manifest styles are attached before activation and removed when that module version leaves the
catalog. A failed script, stylesheet, compatibility check, activation or component render is
reported for that module while authentication, core pages and other modules remain usable.
Module CSS must scope every selector below `[data-union-module="<module-id>"]` or a unique
module-root class. CSS is a trusted extension mechanism rather than a security sandbox, so
untrusted presentation code needs an iframe or a separately deployed application instead of this
in-page plugin contract.

Frontend permission filtering is a usability boundary, not the authorization boundary. Core
enforces session/RBAC on Manifest routes marked `platform`; routes marked `module` must enforce
their Agent, device, ACL or media-domain credentials in the worker. Missing platform permission
grants hide protected menu items and reject protected routes.

## Business-code boundary

The Shell has no built-in Host, Sunshine or Agent-activation route and no fallback module registry.
Every business page and stylesheet comes from an enabled package that Builder included in the
active release. Legacy business UI source files may remain in this repository during code migration,
but the production Shell entry graph
does not import them. A public onboarding page must therefore be delivered through an explicitly
designed public plugin-discovery contract; it must not be silently compiled back into the Shell.
