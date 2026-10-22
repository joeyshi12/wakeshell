# wakeshell

The desktop shell for my [dwl](https://github.com/joeyshi12/dwl) session, written in Rust with
GTK 4 and gtk4-layer-shell. One process provides:

- the wallpaper on every monitor, with a fade when it changes
- the bar: dwl's tags, layout symbol and window title (over dwl's ipc protocol), plus CPU load,
  memory, Wi-Fi and the clock on the selected monitor
- the app launcher
- notifications (`org.freedesktop.Notifications`)
- the volume popup
- the power menu and the polkit password dialog, over a blurred copy of the screen
- the screen-share picker for xdg-desktop-portal-wlr

The lock screen is a separate program, [wakelock](https://github.com/joeyshi12/wakelock).

## Installing

Needs Rust, `gtk4` and `gtk4-layer-shell`:

```sh
cargo install --locked --git https://github.com/joeyshi12/wakeshell
```

This installs to `~/.cargo/bin` (`--path .` installs a local checkout). At runtime it uses `grim`
(screenshots for the blur and share previews) and `wpctl` (the volume popup). The bar needs dwl
built with the ipc patch.

## Running

Start `wakeshell` with the session, for example as a systemd user service that is part of
`graphical-session.target`. Everything else talks to it through the `wakeshell` command:

| Command | Does |
|---|---|
| `wakeshell volume up\|down\|mute` | changes the default sink's volume by 5% with wpctl and shows the popup (the volume still changes if the shell is not running) |
| `wakeshell msg launcher toggle\|open\|close` | the app launcher |
| `wakeshell msg powermenu toggle\|open\|close` | the power menu |
| `wakeshell msg osd volume` | shows the default sink's volume |
| `wakeshell msg wallpaper reload` | fades to the image the wallpaper link points at now |

For screen sharing, set this in `~/.config/xdg-desktop-portal-wlr/config`:

```ini
[screencast]
chooser_type=dmenu
chooser_cmd=wakeshell share-chooser
```

## Configuration

All files are optional and live in `$XDG_CONFIG_HOME/wakeshell`:

- `colors.css`: the palette, as `@define-color NAME #rrggbb;` for `bg`, `bg_alt`, `accent`, `fg`,
  `fg_dim`, `fg_muted`, `urgent`, `negative`, `positive` and `shadow`. Sizes and the rest of the
  look are in `src/style.css`.
- `wakeshell.conf`: `key = value` lines. Commands are split on spaces.

  | Key | Default |
  |---|---|
  | `wallpaper` | `~/.local/share/wallpaper` |
  | `wallpaper_fallback` | none |
  | `tags` | `globe terminal palette message-circle key-round` (one icon per dwl tag) |
  | `monocle_layout` | `2` (index in dwl's `layouts[]`) |
  | `terminal` | `uwsm-app -- ghostty` (middle click on the status) |
  | `launch_prefix` | `uwsm-app --` (empty launches apps directly) |
  | `lock`, `logout`, `suspend`, `reboot`, `poweroff` | `loginctl lock-session`, `uwsm stop`, `systemctl suspend`, `systemctl reboot`, `systemctl poweroff` |

- `icons/NAME.svg`: replaces or adds an icon. The built-in ones are in `icons/`.

Launch counts, which put the most used apps first, are kept in
`$XDG_STATE_HOME/wakeshell/launcher.json`.
