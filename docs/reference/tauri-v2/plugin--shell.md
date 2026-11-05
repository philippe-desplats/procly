# Shell

> Vendored from <https://github.com/tauri-apps/tauri-docs/blob/b9dd7032c3a872cad5995bc78c2946f087019942/src/content/docs/plugin/shell.mdx>
> (published at <https://v2.tauri.app/plugin/shell/>) by `scripts/vendor-docs.py`, MIT licensed.
> Do not edit by hand, regenerate instead.

Access the system shell. Allows you to spawn child processes.

## Supported Platforms

_This plugin requires a Rust version of at least **1.90**._

| Platform | Level | Notes |
| --- | --- | --- |
| windows | full |  |
| linux | full |  |
| macos | full |  |
| android | partial | Only allows to open URLs via `open` |
| ios | partial | Only allows to open URLs via `open` |

## Opener

If you're looking for documentation for the `shell.open` API, check out the new [Opener plugin](../opener/) instead.

## Setup

Install the shell plugin to get started.

**Automatic:**
		Use your project's package manager to add the dependency:

    	{ ' ' }

    	```sh
cargo tauri add shell
npm run tauri add shell
```
          1. Run the following command in the `src-tauri` folder to add the plugin to the project's dependencies in `Cargo.toml`:

              ```sh frame=none
              cargo add tauri-plugin-shell
              ```

          2. Modify `lib.rs` to initialize the plugin:

              ```rust title="src-tauri/src/lib.rs" ins={4}
              #[cfg_attr(mobile, tauri::mobile_entry_point)]
              pub fn run() {
                  tauri::Builder::default()
                      .plugin(tauri_plugin_shell::init())
                      .run(tauri::generate_context!())
                      .expect("error while running tauri application");
              }
              ```

          3. Install the JavaScript Guest bindings using your preferred JavaScript package manager:

              

## Usage

The shell plugin is available in both JavaScript and Rust.

**JavaScript:**

```javascript
import { Command } from '@tauri-apps/plugin-shell';
// when using `"withGlobalTauri": true`, you may use
// const { Command } = window.__TAURI__.shell;

let result = await Command.create('exec-sh', [
  '-c',
  "echo 'Hello World!'",
]).execute();
console.log(result);
```

```rust
use tauri_plugin_shell::ShellExt;

let shell = app_handle.shell();
let output = tauri::async_runtime::block_on(async move {
		shell
				.command("echo")
				.args(["Hello from Rust!"])
				.output()
				.await
				.unwrap()
});
if output.status.success() {
		println!("Result: {:?}", String::from_utf8(output.stdout));
} else {
		println!("Exit with code: {}", output.status.code().unwrap());
}
```

## Permissions

By default all potentially dangerous plugin commands and scopes are blocked and cannot be accessed. You must modify the permissions in your `capabilities` configuration to enable these.

See the [Capabilities Overview](https://v2.tauri.app/security/capabilities/) for more information and the [step by step guide](https://v2.tauri.app/learn/security/using-plugin-permissions/) to use plugin permissions.

```json title="src-tauri/capabilities/default.json" ins={6-23}
{
  "$schema": "../gen/schemas/desktop-schema.json",
  "identifier": "main-capability",
  "description": "Capability for the main window",
  "windows": ["main"],
  "permissions": [
    {
      "identifier": "shell:allow-execute",
      "allow": [
        {
          "name": "exec-sh",
          "cmd": "sh",
          "args": [
            "-c",
            {
              "validator": "\\S+"
            }
          ],
          "sidecar": false
        }
      ]
    }
  ]
}
```

### Default Permission

This permission set configures which
shell functionality is exposed by default.

##### Granted Permissions

It allows to use the `open` functionality with a reasonable
scope pre-configured. It will allow opening `http(s)://`,
`tel:` and `mailto:` links.

##### This default permission set includes the following:

- `allow-open`

### Permission Table

<table>
<tr>
<th>Identifier</th>
<th>Description</th>
</tr>

<tr>
<td>

`shell:allow-execute`

</td>
<td>

Enables the execute command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`shell:deny-execute`

</td>
<td>

Denies the execute command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`shell:allow-kill`

</td>
<td>

Enables the kill command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`shell:deny-kill`

</td>
<td>

Denies the kill command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`shell:allow-open`

</td>
<td>

Enables the open command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`shell:deny-open`

</td>
<td>

Denies the open command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`shell:allow-spawn`

</td>
<td>

Enables the spawn command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`shell:deny-spawn`

</td>
<td>

Denies the spawn command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`shell:allow-stdin-write`

</td>
<td>

Enables the stdin_write command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`shell:deny-stdin-write`

</td>
<td>

Denies the stdin_write command without any pre-configured scope.

</td>
</tr>
</table>
