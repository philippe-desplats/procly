# Runtime Authority

> Vendored from <https://github.com/tauri-apps/tauri-docs/blob/b9dd7032c3a872cad5995bc78c2946f087019942/src/content/docs/security/runtime-authority.mdx>
> (published at <https://v2.tauri.app/security/runtime-authority/>) by `scripts/vendor-docs.py`, MIT licensed.
> Do not edit by hand, regenerate instead.

The runtime authority is part of the Tauri Core.
It holds all permissions, capabilities and scopes at runtime to enforce
which window can access which command and passes scopes to commands.

Whenever a Tauri command is invoked from the webview the runtime authority
receives the invoke request, makes sure that the origin is allowed to actually
use the requested command, checks if the origin is part of capabilities and
if scopes are defined for the command and applicable then they are injected into
the invoke request, which is then passed to the proper Tauri command.

If the origin is not allowed to call the command, the runtime authority will deny
the request and the Tauri command is never invoked.

![IPC Diagram](@assets/concept/runtime-authority.svg)
