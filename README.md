<p align="center">
  <img src="assets/island.png" width="88" alt="">
</p>

<h1 align="center">Dynamic Island for Windows</h1>

<p align="center">
  A small pill at the top of your screen, like the iPhone's Dynamic Island: what's playing, your Claude Code sessions, volume and mic/camera use, at a glance.
</p>

<p align="center">
  <a href="https://github.com/nightdevx/dynamic-island-windows/releases/latest"><img src="https://img.shields.io/github/v/release/nightdevx/dynamic-island-windows?label=download" alt="Download"></a>
  <img src="https://img.shields.io/badge/Windows-10%20%7C%2011-0078D4" alt="Windows 10 | 11">
  <img src="https://img.shields.io/badge/Rust-native-B7410E" alt="Rust">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-green" alt="MIT license"></a>
</p>

<p align="center">
  <b>English</b> · <a href="README.tr.md">Türkçe</a>
</p>

<p align="center">
  <img src="docs/compact-music.png" alt="Closed island playing music">
  &nbsp;
  <img src="docs/compact-claude.png" alt="Closed island showing a Claude Code session waiting for permission">
</p>
<p align="center">
  <img src="docs/expanded-music.png" alt="Open island, music page">
  &nbsp;
  <img src="docs/expanded-claude.png" alt="Open island, Claude page">
</p>

## Features

- **Now playing:** any player that shows up in Windows' media controls (Spotify, browsers, Media Player, Apple Music…). Title, artist, cover art and progress. Previous, play/pause, next, shuffle and repeat buttons when the player supports them. Click the progress bar to seek, scroll to switch between players. While music plays, the closed island shows a small equalizer and a soft glow in the color of the cover.
- **Claude Code:** open sessions and what they are doing (working, waiting for permission, answer ready), plus your 5-hour and weekly plan limits. A session that needs you opens a short peek. Click a session to bring its window to the front.
- **Volume:** a short peek with the level whenever the system volume changes.
- **Microphone and camera:** while an app uses them, the island grows a dot, orange for the microphone and green for the camera.
- **Out of the way:** hides by itself in fullscreen apps and games, and stays above other windows otherwise.

<p align="center">
  <img src="docs/peek.png" alt="A peek: a session waiting for permission">
  &nbsp;
  <img src="docs/volume.png" alt="Volume peek">
</p>

## Light on your PC

The island is a native Rust app with no runtime or browser engine. Every animation runs in Windows' own compositor (DWM), and the app draws only when something it shows changes. When nothing changes it sleeps and uses next to no CPU; memory stays around 10 MB. The installer is 2 MB.

## Install

Download `DynamicIsland-Setup-<version>.exe` from [Releases](https://github.com/nightdevx/dynamic-island-windows/releases/latest) and run it. No administrator rights are needed; it installs for your user only. During setup you can choose:

- **Start with Windows**
- **Connect to Claude Code** (offered when Claude Code is installed): adds the island's hooks to `~/.claude/settings.json` so the Claude tab sees your sessions. Everything else in the file stays as it is, and a backup is saved next to it the first time. Uninstalling removes the hooks again.

The installer isn't code-signed yet, so Windows SmartScreen may warn you: choose **More info → Run anyway**.

Requires Windows 10 (1809) or Windows 11, 64-bit. The interface is in Turkish.

## Use

- Hover over the island, or click it, to open it. The tabs at the top switch between Music and Claude.
- Scroll over the island to switch to another player.
- Right-click the island or its tray icon for: start with Windows, settings, exit.

### Settings

Settings live in `%APPDATA%\DynamicIsland\settings.ini` (right-click → settings) and apply as soon as you save.

| Setting | Default | |
|---|---|---|
| `expand_on_hover` | `true` | Open on hover; `false` opens only on click |
| `hover_delay_ms` | `300` | How long to hover before it opens (ms) |
| `hide_exe` | empty | Hide while these programs are in front, e.g. `valorant.exe, obs64.exe` |
| `show_exe` | empty | Stay visible even when these go fullscreen |
| `equalizer` | `true` | Moving equalizer bars while music plays |

### Claude Code by hand

If you skipped the setup option, add this to `~/.claude/settings.json` (replace `<user>` with your Windows user name):

<details>
<summary>hooks</summary>

```json
{
  "hooks": {
    "SessionStart":      [{ "hooks": [{ "type": "command", "command": "C:/Users/<user>/AppData/Local/DynamicIsland/island.exe", "args": ["--hook"], "timeout": 5 }] }],
    "UserPromptSubmit":  [{ "hooks": [{ "type": "command", "command": "C:/Users/<user>/AppData/Local/DynamicIsland/island.exe", "args": ["--hook"], "async": true }] }],
    "PostToolUse":       [{ "hooks": [{ "type": "command", "command": "C:/Users/<user>/AppData/Local/DynamicIsland/island.exe", "args": ["--hook"], "async": true }] }],
    "PermissionRequest": [{ "hooks": [{ "type": "command", "command": "C:/Users/<user>/AppData/Local/DynamicIsland/island.exe", "args": ["--hook"], "async": true }] }],
    "Notification":      [{ "matcher": "permission_prompt", "hooks": [{ "type": "command", "command": "C:/Users/<user>/AppData/Local/DynamicIsland/island.exe", "args": ["--hook"], "async": true }] }],
    "Stop":              [{ "hooks": [{ "type": "command", "command": "C:/Users/<user>/AppData/Local/DynamicIsland/island.exe", "args": ["--hook"], "async": true }] }],
    "SessionEnd":        [{ "hooks": [{ "type": "command", "command": "C:/Users/<user>/AppData/Local/DynamicIsland/island.exe", "args": ["--hook"], "timeout": 5 }] }]
  }
}
```

</details>

Or run `island.exe --setup-claude`, which does the same as the installer option (`--remove-claude` undoes it).

## Privacy

Nothing leaves your PC except one request: to show your plan limits, the island reads the sign-in Claude Code saved (`~/.claude/.credentials.json`) and asks Anthropic's API for your usage. The token goes only to `api.anthropic.com` over HTTPS, is never stored or logged, and is used at most every 3 minutes, only while you use Claude.

Microphone and camera use comes from Windows' own privacy records, the same ones behind the microphone icon in the taskbar.

## Uninstall

Settings → Apps → Dynamic Island → Uninstall. The app, its logs, the autostart entry and the Claude Code hooks are removed; your settings file stays.

## Build from source

You need Rust (stable, MSVC toolchain) and, for the installer, [Inno Setup 6](https://jrsoftware.org/isinfo.php).

```powershell
cargo build --release                      # island.exe
cargo test
powershell -File scripts\package.ps1       # installer: dist\DynamicIsland-Setup-<version>.exe
powershell -File scripts\make-icon.ps1     # redraws assets\island.ico
```

How it is put together:

| | |
|---|---|
| `src/window.rs` | The window, its messages and what happens on each event |
| `src/island.rs` | The composition tree: springs, fades, every visual |
| `src/pages.rs`, `src/render.rs` | What gets drawn (Direct2D/DirectWrite) |
| `src/media.rs` | Windows media sessions, on their own thread |
| `src/claude.rs`, `src/usage.rs` | Claude Code hooks, sessions and plan limits |
| `src/audio.rs`, `src/privacy.rs` | Volume, microphone/camera |
| `src/json.rs` | Editing Claude Code's settings without reordering them |
| `src/fullscreen.rs` | When to hide |

## Contact

nightdijital · [nightdijital.com](https://nightdijital.com) · [iletisim@nightdijital.com](mailto:iletisim@nightdijital.com)

Released under the [MIT license](LICENSE).
