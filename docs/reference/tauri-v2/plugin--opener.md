# Opener

> Vendored from <https://github.com/tauri-apps/tauri-docs/blob/b9dd7032c3a872cad5995bc78c2946f087019942/src/content/docs/plugin/opener.mdx>
> (published at <https://v2.tauri.app/plugin/opener/>) by `scripts/vendor-docs.py`, MIT licensed.
> Do not edit by hand, regenerate instead.

This plugin allows you to open files and URLs in a specified, or the default, application. It also supports "revealing" files in the system's file explorer.

## Supported Platforms

_This plugin requires a Rust version of at least **1.90**._

| Platform | Level | Notes |
| --- | --- | --- |
| windows | full |  |
| linux | full |  |
| macos | full |  |
| android | partial | Only allows to open URLs via `open` |
| ios | partial | Only allows to open URLs via `open` |

## Setup

Install the opener plugin to get started.

**Automatic:**
		Use your project's package manager to add the dependency:

    	{ ' ' }

    	```sh
cargo tauri add opener
npm run tauri add opener
```
          1. Run the following command in the `src-tauri` folder to add the plugin to the project's dependencies in `Cargo.toml`:

              ```sh frame=none
              cargo add tauri-plugin-opener
              ```

          2. Modify `lib.rs` to initialize the plugin:

              ```rust title="src-tauri/src/lib.rs" ins={4}
              #[cfg_attr(mobile, tauri::mobile_entry_point)]
              pub fn run() {
                  tauri::Builder::default()
                      .plugin(tauri_plugin_opener::init())
                      .run(tauri::generate_context!())
                      .expect("error while running tauri application");
              }
              ```

          3. Install the JavaScript Guest bindings using your preferred JavaScript package manager:

              

## Usage

The opener plugin is available in both JavaScript and Rust.

**JavaScript:**

```javascript
import { openPath, openUrl } from '@tauri-apps/plugin-opener';
// when using `"withGlobalTauri": true`, you may use
// const { openPath } = window.__TAURI__.opener;

// opens a file using the default program:
await openPath('/path/to/file');
// opens a file using `vlc` command on Windows:
await openPath('C:/path/to/file', 'vlc');
// opens a URL using the default program:
await openUrl('https://tauri.app');
```

Note that `app` is an instance of `App` or [`AppHandle`](https://docs.rs/tauri/2.0.0/tauri/struct.AppHandle.html).

```rust
use tauri_plugin_opener::OpenerExt;

// opens a file using the default program:
app.opener().open_path("/path/to/file", None::<&str>);
// opens a file using `vlc` command on Windows:
app.opener().open_path("C:/path/to/file", Some("vlc"));
// opens a URL using the default program:
app.opener().open_url("https://tauri.app", None::<&str>);
```

## Permissions

By default all potentially dangerous plugin commands and scopes are blocked and cannot be accessed. You must modify the permissions in your `capabilities` configuration to enable these.

See the [Capabilities Overview](https://v2.tauri.app/security/capabilities/) for more information and the [step by step guide](https://v2.tauri.app/learn/security/using-plugin-permissions/) to use plugin permissions.

Below are two example scope configurations. Both `path` and `url` use the [glob pattern syntax](https://docs.rs/glob/latest/glob/struct.Pattern.html) to define allowed file paths and URLs.

First, an example on how to add permissions to specific paths for the `openPath()` function:

```json title="src-tauri/capabilities/default.json" ins={6-15}
{
  "$schema": "../gen/schemas/desktop-schema.json",
  "identifier": "main-capability",
  "description": "Capability for the main window",
  "windows": ["main"],
  "permissions": [
    {
      "identifier": "opener:allow-open-path",
      "allow": [
        {
          "path": "/path/to/file"
        },
        {
          "path": "$APPDATA/file"
        }
      ]
    }
  ]
}
```

Lastly, an example on how to add permissions for the exact `https://tauri.app` URL and all URLs on a custom protocol (must be known to the OS) for the `openUrl()` function:

```json title="src-tauri/capabilities/default.json" ins={6-15}
{
  "$schema": "../gen/schemas/desktop-schema.json",
  "identifier": "main-capability",
  "description": "Capability for the main window",
  "windows": ["main"],
  "permissions": [
    {
      "identifier": "opener:allow-open-url",
      "allow": [
        {
          "url": "https://tauri.app"
        },
        {
          "url": "custom:*"
        }
      ]
    }
  ]
}
```

### Default Permission

This permission set allows opening `mailto:`, `tel:`, `https://` and `http://` urls using their default application
as well as reveal file in directories using default file explorer

##### This default permission set includes the following:

- `allow-open-url`
- `allow-reveal-item-in-dir`
- `allow-default-urls`

### Permission Table

<table>
<tr>
<th>Identifier</th>
<th>Description</th>
</tr>

<tr>
<td>

`opener:allow-default-urls`

</td>
<td>

This enables opening `mailto:`, `tel:`, `https://` and `http://` urls using their default application.

</td>
</tr>

<tr>
<td>

`opener:allow-open-path`

</td>
<td>

Enables the open_path command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`opener:deny-open-path`

</td>
<td>

Denies the open_path command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`opener:allow-open-url`

</td>
<td>

Enables the open_url command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`opener:deny-open-url`

</td>
<td>

Denies the open_url command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`opener:allow-reveal-item-in-dir`

</td>
<td>

Enables the reveal_item_in_dir command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`opener:deny-reveal-item-in-dir`

</td>
<td>

Denies the reveal_item_in_dir command without any pre-configured scope.

</td>
</tr>
</table>
