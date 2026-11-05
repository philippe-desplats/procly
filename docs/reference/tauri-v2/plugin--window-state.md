# Window State

> Vendored from <https://github.com/tauri-apps/tauri-docs/blob/b9dd7032c3a872cad5995bc78c2946f087019942/src/content/docs/plugin/window-state.mdx>
> (published at <https://v2.tauri.app/plugin/window-state/>) by `scripts/vendor-docs.py`, MIT licensed.
> Do not edit by hand, regenerate instead.

Save window positions and sizes and restore them when the app is reopened.

## Supported Platforms

_This plugin requires a Rust version of at least **1.90**._

| Platform | Level | Notes |
| --- | --- | --- |
| windows | full |  |
| linux | full |  |
| macos | full |  |
| android | none |  |
| ios | none |  |

## Setup

Install the window-state plugin to get started.

**Automatic:**

Use your project's package manager to add the dependency:

{' '}

```sh
cargo tauri add window-state
npm run tauri add window-state
```

**Manual:**

    1. Run the following command in the `src-tauri` folder to add the plugin to the project's dependencies in `Cargo.toml`:

        ```sh frame=none
        cargo add tauri-plugin-window-state --target 'cfg(any(target_os = "macos", windows, target_os = "linux"))'
        ```

    2.  Modify `lib.rs` to initialize the plugin:

        ```rust title="src-tauri/src/lib.rs" ins={4}
        #[cfg_attr(mobile, tauri::mobile_entry_point)]
        pub fn run() {
            tauri::Builder::default()
                .setup(|app| {
                    #[cfg(desktop)]
                    app.handle().plugin(tauri_plugin_window_state::Builder::default().build());
                    Ok(())
                })
                .run(tauri::generate_context!())
                .expect("error while running tauri application");
        }
        ```

    3.  Install the JavaScript Guest bindings using your preferred JavaScript package manager:

        ```sh
npm install @tauri-apps/plugin-window-state
```

## Usage

After adding the window-state plugin, all windows will remember their state when the app is being closed and will restore to their previous state on the next launch.

You can also access the window-state plugin in both JavaScript and Rust.

:::tip
Restoring the state will happen after window creation,
so to prevent the window from flashing, you can set `visible` to `false` when creating the window,
the plugin will show the window when it restores the state
:::

### JavaScript

You can use `saveWindowState` to manually save the window state:

```javascript
import { saveWindowState, StateFlags } from '@tauri-apps/plugin-window-state';
// when using `"withGlobalTauri": true`, you may use
// const { saveWindowState, StateFlags } = window.__TAURI__.windowState;

saveWindowState(StateFlags.ALL);
```

Similarly you can manually restore a window's state from disk:

```javascript
import {
  restoreStateCurrent,
  StateFlags,
} from '@tauri-apps/plugin-window-state';
// when using `"withGlobalTauri": true`, you may use
// const { restoreStateCurrent, StateFlags } = window.__TAURI__.windowState;

restoreStateCurrent(StateFlags.ALL);
```

### Rust

You can use the `save_window_state()` method exposed by the `AppHandleExt` trait:

```rust
use tauri_plugin_window_state::{AppHandleExt, StateFlags};

// `tauri::AppHandle` now has the following additional method
app.save_window_state(StateFlags::all()); // will save the state of all open windows to disk
```

Similarly you can manually restore a window's state from disk using the `restore_state()` method exposed by the `WindowExt` trait:

```rust
use tauri_plugin_window_state::{WindowExt, StateFlags};

// all `Window` types now have the following additional method
window.restore_state(StateFlags::all()); // will restore the window's state from disk
```

## Permissions

By default all potentially dangerous plugin commands and scopes are blocked and cannot be accessed. You must modify the permissions in your `capabilities` configuration to enable these.

See the [Capabilities Overview](https://v2.tauri.app/security/capabilities/) for more information and the [step by step guide](https://v2.tauri.app/learn/security/using-plugin-permissions/) to use plugin permissions.

```json title="src-tauri/capabilities/default.json" ins={4}
{
  "permissions": [
    ...,
    "window-state:default",
  ]
}
```

### Default Permission

This permission set configures what kind of
operations are available from the window state plugin.

##### Granted Permissions

All operations are enabled by default.

##### This default permission set includes the following:

- `allow-filename`
- `allow-restore-state`
- `allow-save-window-state`

### Permission Table

<table>
<tr>
<th>Identifier</th>
<th>Description</th>
</tr>

<tr>
<td>

`window-state:allow-filename`

</td>
<td>

Enables the filename command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`window-state:deny-filename`

</td>
<td>

Denies the filename command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`window-state:allow-restore-state`

</td>
<td>

Enables the restore_state command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`window-state:deny-restore-state`

</td>
<td>

Denies the restore_state command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`window-state:allow-save-window-state`

</td>
<td>

Enables the save_window_state command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`window-state:deny-save-window-state`

</td>
<td>

Denies the save_window_state command without any pre-configured scope.

</td>
</tr>
</table>
