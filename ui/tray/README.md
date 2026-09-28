# Whispy — StatusNotifierItem (SNI) Tray Indicator

A lightweight, zero-configuration system tray indicator for `whispy-daemon`. It connects to the freedesktop StatusNotifierItem DBus specification (`org.kde.StatusNotifierItem`), providing dynamic visual feedback on any desktop shell or panel (Noctalia, Waybar, KDE Plasma, XFCE, i3bar, Polybar, etc.) across both **Wayland** and **X11**.

---

## Visual States

The indicator reflects the real-time state published by `whispy-daemon` in `$XDG_RUNTIME_DIR/whispy/state.json`:

| State | Icon | Description |
|---|---|---|
| `idle` | 🎙️ Microphone (`whispy-idle.svg`) | Daemon is ready and waiting for push-to-talk. |
| `recording` | 👂 Ear (`whispy-recording.svg`) | Listening and buffering audio from PipeWire. |
| `transcribing` | 🧠 Brain (`whispy-transcribing.svg`) | Transcribing captured audio with `whisper.cpp` on GPU/CPU. |
| `error` | ⚠️ Mic off (`whispy-error.svg`) | Capture or STT error (hover tooltip details the error). |

---

## Features

- **Dynamic Vector Assets**: Bundled scalable SVG icons matching the Tabler design system with clean 2px rounded strokes.
- **Pre-rendered ARGB32 Pixmaps**: Provides both freedesktop `IconName` and `IconPixmap` buffers for instant, crisp rendering on all tray hosts without theme caching delays.
- **Mouse Controls**:
  - **Left Click**: Toggle capture (`whispy-client toggle`).
  - **Middle Click**: Cancel current capture (`whispy-client cancel`).
  - **Right Click**: Open native context menu (`com.canonical.dbusmenu`).
- **Interactive Context Menu (Right Click)**:
  - **Live Status Header**: Displays the current daemon activity.
  - **Start / Stop Dictation**: Toggle capture directly from the menu.
  - **Cancel Dictation**: Abort the active capture and discard buffered audio.
  - **Mute Audio While Recording**: Automatically ducks/mutes system audio output (`wpctl` / `pactl`) while capturing speech to prevent desktop audio (music, videos, voice calls) from bleeding into the microphone buffer. Can be toggled live from the context menu or configured via `mute_playback = true` in `config.toml`.
  - **Recording History Submenu**: Lists the last 10 transcribed recordings. Clicking any entry copies the full transcript to the clipboard (`wl-copy` / `xclip`) with a toast notification. Includes a shortcut to open the full `transcripts.jsonl` log.
  - **Edit Configuration**: Quick shortcut to open `~/.config/whispy/config.toml` in your default editor.
  - **Restart Daemon**: Restart `whispy-daemon.service` with one click.
  - **Quit**: Stop the tray companion.
- **Interactive Tooltip**: Hovering over the icon displays the current status and helpful shortcuts.

---

## Dependencies

- Python 3
- `python-gobject` (PyGObject, for DBus / GLib integration)
- `librsvg` (for SVG vector rendering)

On Arch Linux / CachyOS:
```bash
sudo pacman -S python-gobject librsvg
```

On Debian / Ubuntu:
```bash
sudo apt install python3-gi gir1.2-rsvg-2.0
```

On Fedora:
```bash
sudo dnf install python3-gobject librsvg2
```

---

## Installation

### 1. Install files

```bash
# From the repository root:
mkdir -p ~/.local/bin ~/.local/share/icons/hicolor/scalable/apps ~/.config/systemd/user

# Install script
cp ui/tray/whispy-tray ~/.local/bin/
chmod +x ~/.local/bin/whispy-tray

# Install icons
cp ui/tray/icons/*.svg ~/.local/share/icons/hicolor/scalable/apps/

# Install systemd service
cp ui/tray/systemd/whispy-tray.service ~/.config/systemd/user/
```

### 2. Enable and start service

```bash
systemctl --user daemon-reload
systemctl --user enable --now whispy-tray.service
```

Verify status with:
```bash
systemctl --user status whispy-tray.service
```
