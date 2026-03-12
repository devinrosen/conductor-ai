# Themes

Conductor TUI supports two ways to configure the color theme.

## Named built-in themes

Set `theme` in `~/.conductor/config.toml`:

```toml
[general]
theme = "nord"
```

Available names: `conductor` (default), `nord`, `gruvbox`, `catppuccin_mocha`.

## Custom base16 TOML file

Set `theme_path` to a path pointing at a [base16](https://github.com/tinted-theming/home)-format TOML file. When `theme_path` is set it takes precedence over `theme`.

```toml
[general]
theme_path = "~/.config/conductor/my-theme.toml"
```

### Minimal example

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

The [tinted-theming](https://tinted-theming.github.io/tinted-theming/) project maintains hundreds of base16-compatible themes you can use directly.
