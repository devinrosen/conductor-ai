# Themes

Conductor TUI supports built-in named themes and custom themes dropped into `~/.conductor/themes/`.

## Named built-in themes

Set `theme` in `~/.conductor/config.toml`:

```toml
[general]
theme = "nord"
```

Available names: `conductor` (default), `nord`, `gruvbox`, `catppuccin_mocha`.

## Custom themes folder

Drop `.toml`, `.yaml`, or `.yml` base16 theme files into `~/.conductor/themes/`. The directory is created automatically on first run.

Custom themes appear in the TUI theme picker (press **T**) alongside built-ins. The picker rescans the folder each time it opens, so newly added files are available without restarting.

To use a custom theme on startup, set its filename stem (without extension) as the `theme` value:

```toml
[general]
theme = "my-theme"   # loads ~/.conductor/themes/my-theme.toml (or .yaml/.yml)
```

### TOML format (conductor-native)

Compatible with [tinted-theming/base16](https://github.com/tinted-theming/home):

```toml
base00 = "#1d2021"
base01 = "#282828"
base02 = "#32302f"
base03 = "#504945"
base04 = "#bdae93"
base05 = "#d5c4a1"
base06 = "#ebdbb2"
base07 = "#fbf1c7"
base08 = "#fb4934"
base09 = "#fe8019"
base0A = "#fabd2f"
base0B = "#b8bb26"
base0C = "#8ec07c"
base0D = "#83a598"
base0E = "#d3869b"
base0F = "#d65d0e"
```

Hex values may optionally include the `#` prefix. All required slots must be present.

The display label in the picker is the filename stem (e.g. `my-theme`).

### YAML format (tinted-theming community files)

Compatible with the [tinted-theming/base16-schemes](https://github.com/tinted-theming/base16-schemes) repository:

```yaml
system: base16
name: "My Theme"
author: "Author Name"
variant: dark
palette:
  base00: "1d2021"
  base01: "282828"
  base02: "32302f"
  base03: "504945"
  base04: "bdae93"
  base05: "d5c4a1"
  base06: "ebdbb2"
  base07: "fbf1c7"
  base08: "fb4934"
  base09: "fe8019"
  base0A: "fabd2f"
  base0B: "b8bb26"
  base0C: "8ec07c"
  base0D: "83a598"
  base0E: "d3869b"
  base0F: "d65d0e"
```

Hex values have no `#` prefix. The `name:` field is used as the display label in the picker; the filename stem is used as a fallback if `name:` is absent.

### Base16 → semantic role mapping

| Base16 slot | Semantic roles |
|-------------|---------------|
| `base02`    | `highlight_bg` (selected row background) |
| `base03`    | `label_secondary`, `border_inactive`, `status_cancelled` |
| `base05`    | `label_primary` (workflow names, PR titles) |
| `base08`    | `status_failed`, `label_error` |
| `base0A`    | `status_running`, `label_warning`, `label_accent` |
| `base0B`    | `status_completed` |
| `base0C`    | `border_focused`, `group_header` |
| `base0D`    | `label_info`, `label_url` |
| `base0E`    | `status_waiting` |

Slots `base00`, `base01`, `base04`, `base06`, `base07`, `base09`, `base0F` are parsed from the file but not currently mapped to a role. They may be used in future releases.

### Ready-made themes

The [tinted-theming](https://tinted-theming.github.io/tinted-theming/) project maintains hundreds of base16-compatible YAML themes. Download any `.yaml` file from [base16-schemes](https://github.com/tinted-theming/base16-schemes) and place it in `~/.conductor/themes/`.
