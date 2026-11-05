# Positioner

> Vendored from <https://github.com/tauri-apps/tauri-docs/blob/b9dd7032c3a872cad5995bc78c2946f087019942/src/content/docs/plugin/positioner.mdx>
> (published at <https://v2.tauri.app/plugin/positioner/>) by `scripts/vendor-docs.py`, MIT licensed.
> Do not edit by hand, regenerate instead.

Position your windows at well-known locations.

This plugin is a port of [electron-positioner](https://github.com/jenslind/electron-positioner) for Tauri.

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

Install the positioner plugin to get started.

:::note
If you only intend on moving the window from Rust code, you only need the dependency in `src-tauri/Cargo.toml`, and can remove the plugin registration from `lib.rs` if you choose to setup automatically.
:::

**Automatic:**

    	Use your project's package manager to add the dependency:

    	{ ' ' }

    	```sh
cargo tauri add positioner
npm run tauri add positioner
```

    			1. Run the following command in the `src-tauri` folder to add the plugin to the project's dependencies in `Cargo.toml`:

    				```sh frame=none
    				cargo add tauri-plugin-positioner --target 'cfg(any(target_os = "macos", windows, target_os = "linux"))'
    				```

    			2. Modify `lib.rs` to initialize the plugin:

    				```rust title="src-tauri/src/lib.rs" ins={4}
    				#[cfg_attr(mobile, tauri::mobile_entry_point)]
    				pub fn run() {
    						tauri::Builder::default()
    								.setup(|app| {
    										#[cfg(desktop)]
    										app.handle().plugin(tauri_plugin_positioner::init());
    										Ok(())
    								})
    								.run(tauri::generate_context!())
    								.expect("error while running tauri application");
    				}
    				```

    			3. Install the JavaScript Guest bindings using your preferred JavaScript package manager:

    				

Additional setup is required to get tray-relative positions to work.

	1. Add `tray-icon` feature to your `Cargo.toml` file:
		```toml title="src-tauri/Cargo.toml" ins={2}
		[dependencies]
		tauri-plugin-positioner = { version = "2.0.0", features = ["tray-icon"] }
		```

    2. Setup `on_tray_event` for positioner plugin:
    	```rust title="src-tauri/src/lib.rs" ins={4-12}
    	pub fn run() {
    		tauri::Builder::default()
    			// This is required to get tray-relative positions to work
    			.setup(|app| {
    					#[cfg(desktop)]
    					{
    						app.handle().plugin(tauri_plugin_positioner::init());
    							tauri::tray::TrayIconBuilder::new()
    								.on_tray_icon_event(|tray_handle, event| {
    									tauri_plugin_positioner::on_tray_event(tray_handle.app_handle(), &event);
    								})
    								.build(app)?;
    					}
    				Ok(())
    			})
    			.run(tauri::generate_context!())
    			.expect("error while running tauri application");
    	}
    	```

## Usage

The plugin's APIs are available through the JavaScript guest bindings:

```javascript
import { moveWindow, Position } from '@tauri-apps/plugin-positioner';
// when using `"withGlobalTauri": true`, you may use
// const { moveWindow, Position } = window.__TAURI__.positioner;

moveWindow(Position.TopRight);
```

You can import and use the Window trait extension directly through Rust:

```rust
use tauri_plugin_positioner::{WindowExt, Position};

let mut win = app.get_webview_window("main").unwrap();
let _ = win.as_ref().window().move_window(Position::TopRight);
```

## Permissions

By default all potentially dangerous plugin commands and scopes are blocked and cannot be accessed. You must modify the permissions in your `capabilities` configuration to enable these.

See the [Capabilities Overview](https://v2.tauri.app/security/capabilities/) for more information and the [step by step guide](https://v2.tauri.app/learn/security/using-plugin-permissions/) to use plugin permissions.

```json title="src-tauri/capabilities/default.json" ins={4}
{
  "permissions": [
    ...,
    "positioner:default",
  ]
}
```

### Default Permission

Allows the moveWindow and handleIconState APIs

##### This default permission set includes the following:

- `allow-move-window`
- `allow-move-window-constrained`
- `allow-set-tray-icon-state`

### Permission Table

<table>
<tr>
<th>Identifier</th>
<th>Description</th>
</tr>

<tr>
<td>

`positioner:allow-move-window`

</td>
<td>

Enables the move_window command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`positioner:deny-move-window`

</td>
<td>

Denies the move_window command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`positioner:allow-move-window-constrained`

</td>
<td>

Enables the move_window_constrained command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`positioner:deny-move-window-constrained`

</td>
<td>

Denies the move_window_constrained command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`positioner:allow-set-tray-icon-state`

</td>
<td>

Enables the set_tray_icon_state command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`positioner:deny-set-tray-icon-state`

</td>
<td>

Denies the set_tray_icon_state command without any pre-configured scope.

</td>
</tr>
</table>
