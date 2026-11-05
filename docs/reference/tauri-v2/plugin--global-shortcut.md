# Global Shortcut

> Vendored from <https://github.com/tauri-apps/tauri-docs/blob/b9dd7032c3a872cad5995bc78c2946f087019942/src/content/docs/plugin/global-shortcut.mdx>
> (published at <https://v2.tauri.app/plugin/global-shortcut/>) by `scripts/vendor-docs.py`, MIT licensed.
> Do not edit by hand, regenerate instead.

Register global shortcuts.

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

Install the global-shortcut plugin to get started.

**Automatic:**

    Use your project's package manager to add the dependency:

    { ' ' }

    ```sh
cargo tauri add global-shortcut
npm run tauri add global-shortcut
```

    1. Run the following command in the `src-tauri` folder to add the plugin to the project's dependencies in `Cargo.toml`:

        ```sh frame=none
        cargo add tauri-plugin-global-shortcut --target 'cfg(any(target_os = "macos", windows, target_os = "linux"))'
        ```

    2.  Modify `lib.rs` to initialize the plugin:

        ```rust title="src-tauri/src/lib.rs" ins={4-5}
        pub fn run() {
            tauri::Builder::default()
                .setup(|app| {
                    #[cfg(desktop)]
                    app.handle().plugin(tauri_plugin_global_shortcut::Builder::new().build());
                    Ok(())
                })
                .run(tauri::generate_context!())
                .expect("error while running tauri application");
        }
        ```

    3.  Install the JavaScript Guest bindings using your preferred JavaScript package manager:

        

## Usage

The global-shortcut plugin is available in both JavaScript and Rust.

**JavaScript:**

```javascript
import { register } from '@tauri-apps/plugin-global-shortcut';
// when using `"withGlobalTauri": true`, you may use
// const { register } = window.__TAURI__.globalShortcut;

await register('CommandOrControl+Shift+C', () => {
  console.log('Shortcut triggered');
});
```

```rust title="src-tauri/src/lib.rs"
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            #[cfg(desktop)]
            {
                use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

                let ctrl_n_shortcut = Shortcut::new(Some(Modifiers::CONTROL), Code::KeyN);
                app.handle().plugin(
                    tauri_plugin_global_shortcut::Builder::new().with_handler(move |_app, shortcut, event| {
                        println!("{:?}", shortcut);
                        if shortcut == &ctrl_n_shortcut {
                            match event.state() {
                              ShortcutState::Pressed => {
                                println!("Ctrl-N Pressed!");
                              }
                              ShortcutState::Released => {
                                println!("Ctrl-N Released!");
                              }
                            }
                        }
                    })
                    .build(),
                )?;

                app.global_shortcut().register(ctrl_n_shortcut)?;
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
```

## Permissions

By default all potentially dangerous plugin commands and scopes are blocked and cannot be accessed. You must modify the permissions in your `capabilities` configuration to enable these.

See the [Capabilities Overview](https://v2.tauri.app/security/capabilities/) for more information and the [step by step guide](https://v2.tauri.app/learn/security/using-plugin-permissions/) to use plugin permissions.

```json title="src-tauri/capabilities/default.json" ins={7-9}
{
  "$schema": "../gen/schemas/desktop-schema.json",
  "identifier": "main-capability",
  "description": "Capability for the main window",
  "windows": ["main"],
  "permissions": [
    "global-shortcut:allow-is-registered",
    "global-shortcut:allow-register",
    "global-shortcut:allow-unregister"
  ]
}
```

### Default Permission

No features are enabled by default, as we believe
the shortcuts can be inherently dangerous and it is
application specific if specific shortcuts should be
registered or unregistered.

### Permission Table

<table>
<tr>
<th>Identifier</th>
<th>Description</th>
</tr>

<tr>
<td>

`global-shortcut:allow-is-registered`

</td>
<td>

Enables the is_registered command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`global-shortcut:deny-is-registered`

</td>
<td>

Denies the is_registered command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`global-shortcut:allow-register`

</td>
<td>

Enables the register command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`global-shortcut:deny-register`

</td>
<td>

Denies the register command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`global-shortcut:allow-register-all`

</td>
<td>

Enables the register_all command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`global-shortcut:deny-register-all`

</td>
<td>

Denies the register_all command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`global-shortcut:allow-unregister`

</td>
<td>

Enables the unregister command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`global-shortcut:deny-unregister`

</td>
<td>

Denies the unregister command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`global-shortcut:allow-unregister-all`

</td>
<td>

Enables the unregister_all command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`global-shortcut:deny-unregister-all`

</td>
<td>

Denies the unregister_all command without any pre-configured scope.

</td>
</tr>
</table>
