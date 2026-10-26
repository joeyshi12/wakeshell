# wakeshell

The desktop shell for my [dwl](https://github.com/joeyshi12/dwl) session, written in Rust with
GTK 4 (through Relm4) and gtk4-layer-shell.

## Installing

Needs Rust, `gtk4` and `gtk4-layer-shell`:

```sh
cargo install --locked --git https://github.com/joeyshi12/wakeshell
```

This installs to `~/.cargo/bin` (`--path .` installs a local checkout). At runtime it uses `grim`
(the blur and share previews) and PipeWire's `wpctl` and `pw-dump` (the volume popup and mixer).
The bar needs dwl built with the ipc patch.

## Running

Start `wakeshell` with the session, for example as a systemd user service that is part of
`graphical-session.target`. Everything else talks to it through the `wakeshell` command:

| Command | Does |
|---|---|
| `wakeshell volume up\|down\|mute` | changes the default sink's volume by 5% with wpctl and shows the popup (the volume still changes if the shell is not running) |
| `wakeshell msg launcher toggle\|open\|close` | the app launcher |
| `wakeshell msg powermenu toggle\|open\|close` | the power menu |
| `wakeshell msg mixer toggle\|open\|close` | the volume mixer, under the bar's volume |
| `wakeshell msg osd volume` | shows the default sink's volume |
| `wakeshell msg wallpaper reload` | loads the wallpaper file again and fades to it |

In the bar, the volume opens the mixer; scrolling over it changes the volume and a middle click
mutes.

For screen sharing, set this in `~/.config/xdg-desktop-portal-wlr/config`:

```ini
[screencast]
chooser_type=dmenu
chooser_cmd=wakeshell share-chooser
```

## Configuration

Colours are CSS variables, which GTK picks up from `~/.config/gtk-4.0/gtk.css`: `--wakenight-bg`,
`-bg-alt`, `-accent`, `-fg`, `-fg-dim`, `-fg-muted`, `-urgent` and `-shadow`, set in `:root { }`.
Unset ones keep the defaults in `src/style.rs`. Sizes and the rest of the look are in
`src/style.css`.

All files are optional and live in `$XDG_CONFIG_HOME/wakeshell`:

- `wakeshell.conf`: `key = value` lines. Commands are split on spaces.

  | Key | Default |
  |---|---|
  | `wallpaper` | none (the `--wakenight-bg` colour, also used if the image fails to load) |
  | `tags` | `globe terminal palette message-circle key-round` (one icon per dwl tag) |
  | `monocle_layout` | `2` (index in dwl's `layouts[]`) |
  | `terminal` | `uwsm-app -- ghostty` (middle click on the status) |
  | `launch_prefix` | `uwsm-app --` (empty launches apps directly) |
  | `lock`, `logout`, `suspend`, `reboot`, `poweroff` | `loginctl lock-session`, `uwsm stop`, `systemctl suspend`, `systemctl reboot`, `systemctl poweroff` |

- `icons/NAME.svg`: replaces or adds an icon. The built-in ones are in `icons/`.

Launch counts, which put the most used apps first, are kept in
`$XDG_STATE_HOME/wakeshell/launcher.json`.
