# Shape and screens of the desktop app (proposal for #82)

Status: proposal, to be validated by the owner before ticket #82 closes. Prototype: `shape-d.html` in this folder. Rejected variants: `shape.html` (A, B, C). This file and both prototypes live on the throwaway branch `prototype/desktop-app-shape`, only the validated decision goes to `main`.

How it was reached: 3 variants, a first expert round (UX, visual design, macOS, developer experience) that rejected all 3 and defined D, then a second round of 4 fresh experts that kept D with changes. This file states the shape after those changes.

## The shape

- One window, the only place where work happens: a full-height sidebar running under the traffic lights, a center pane, and an inspector on the right.
- A menu bar extra whose click opens a native menu (an NSMenu, never a webview popover). The menu lists only what needs the developer, each item opening the window on that item.
- Closing the window stops nothing: the app stays in the menu bar, the Daemon and every Service keep running. Quitting the app stops nothing either.
- The app draws only what the socket protocol returns, and holds no supervision logic (ADR 0009).

## Screens

| Screen | Holds |
| --- | --- |
| Overview, the default (⌘0) | "Needs you" pinned on top, then every Service of every Worktree with its port, then the inspector of the selection |
| Worktree | The Foreign processes, the Services (Process services and Isolated Compose services), the Jobs, and this Worktree's doctor checks, run on demand |
| Inspector | State word, what to do next for the end kind, port, Stop, the copied restart command, the output snapshot |
| Doctor | The checks of the machine and the Daemon, problems first, message as the main line, check name as a caption, each fix with a Copy button, the agent prompt |
| Orphans | Grouped by removed Worktree, the data each one drops, then the result item by item |
| Outside Repositories | The Jobs started from a directory in no git Repository |
| Daemon not running | One "Start the Daemon" button, `procly unit install` to copy, the doctor checks that run without the Daemon |
| Daemon of another version | 2 cases: an older Daemon (confirmed restart through `procly daemon stop`), a newer Daemon (update the app, no restart offered) |
| Settings | Editor and terminal used to open a Worktree, opt-in notification on a failure, global shortcut off |

## Rules the shape follows

Signal

- "Needs you" holds failures not marked as seen and Foreign processes, nothing else. Routine `starting` and `restarting` stay out, and so does doctor, which is a separate signal with its own time.
- An end kind is a failure unless it is a clean one (`stopped`, `idle`, `exited` with code 0). An unknown kind counts as a failure.
- "Mark as Seen" exists for ended failures only. The row stays visible and dimmed, it leaves the count, the menu and "Needs you", and comes back when its state changes. There is no Dismiss and no "Remove from the list": `down` does nothing on an ended Service.
- The menu bar glyph is a monochrome template image in 3 variants: the mark, the mark with a notch when something needs the developer, and an outline when the Daemon cannot be read. There is no coloured badge.
- Freshness is an absolute time ("Updated at 14:01"), never a relative age frozen into a menu.

Actions

- Restart is not an action of the app. It is a command to copy, until #88 decides the environment an `up` started from the app carries: a Service inherits the environment of the `up` that started it, and an app opened from the Dock has neither the shell PATH nor nvm, mise or direnv.
- Stop is offered only on a running Service or Isolated Compose service, in the inspector, the Actions menu, the context menu and ⌘. when the list or the inspector has focus. It is not red, since it is reversible.
- Destructive actions open a sheet that names every effect and the loss in red, shows the CLI equivalent in full, and makes Cancel the default button.
- No Stop All Services.

Copies

- A copy box shows exactly what goes to the clipboard, wrapped rather than truncated.
- A command that takes the Worktree from its working directory is copied as `(cd <path> && procly <verb>)`, since the CLI has no `-C`. `procly wait <job>` and `procly logs <job>` take no directory.
- Clicking a port copies it. ⌘C copies the selection's main value, ⌥⌘C the Worktree path.

Visual grammar

- The glyph slot shows the state only, the kind is said in text. Healthy is a quiet solid dot, ended a hollow ring, restarting a spinning amber ring, a failure a red cross, a running Job a half-filled ring, a Foreign process an amber diamond.
- 3 type sizes: 11, 13 and 17 px.
- Selection, focus and default buttons use the system accent colour. Lime is only the brand mark and the single forward action of a full-window state screen, "Start the Daemon".
- The app follows the system appearance. Contrast holds in both themes.

Platform

- A Procly app menu (About, Settings ⌘,, Hide, Quit), then File, Edit, View, Actions, Worktree, Window, Help. ⌘R refreshes, it never restarts.
- No ⌘1 to ⌘9 on Worktrees, since agents add and remove them.
- Native menus show one line per item and a one-character key equivalent. A port goes in the item title, never in the key column.
- Context menus on list rows and on Worktrees in the sidebar.

Data honesty

- A Worktree is labelled by the basename of its path, the Main worktree is captioned, the slug is in the tooltip and the Copy menu.
- No Leases, no Logical resources, no Shared or Logical Compose services: no Request lists them for a live Worktree.
- Output is a snapshot with the time it was fetched, per item. No follow mode until #86.
- "Open localhost:<port>" assumes `http`, since `Status` carries the port only.

## Left to other tickets

- #88: the environment of an `up` started from the app, and then Restart. A verb that forgets an ended Service or Job record. The depth of each confirmation.
- #86: how the app refreshes, the cost of `Status` for the menu bar, a `Logs` cursor before any follow mode, the opt-in notification on a transition to failure.
- #91: notifications need the signed bundle.
- Map fog: the Settings window, launch at login, `branch` in `WorktreeStatus` for opaque Worktree directories, the retention of ended Jobs in the Daemon.
- Task: vendor the missing Tauri v2 pages (system tray, window customization, notification, opener, shell). Every claim on template glyphs, activation policy, reopen and notifications stays unproven until then.
