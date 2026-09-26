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
  - **Right Click**: Toggle capture (`whispy-client toggle`).
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
