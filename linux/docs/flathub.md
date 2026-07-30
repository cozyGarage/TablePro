# Flathub submission

TablePro Linux ships as Flatpak (`com.tablepro.linux`). This note is the
submission checklist for Flathub.

## Manifest

- App ID: `com.tablepro.linux`
- Manifest: [`../flatpak/com.tablepro.linux.json`](../flatpak/com.tablepro.linux.json)
- Metainfo: [`../flatpak/com.tablepro.linux.metainfo.xml`](../flatpak/com.tablepro.linux.metainfo.xml)
- Desktop file: [`../flatpak/com.tablepro.linux.desktop`](../flatpak/com.tablepro.linux.desktop)

## Screenshots

Place PNG screenshots under `flatpak/screenshots/` (1280×800 or larger):

| File | Subject |
|---|---|
| `01-welcome.png` | Welcome / saved connections |
| `02-browse.png` | Table browse with grid |
| `03-editor.png` | SQL editor with results |
| `04-structure.png` | Structure / DDL editor |

Reference them from metainfo `<screenshots>` once captured on GNOME.

## Submit

1. Fork [flathub/flathub](https://github.com/flathub/flathub) and open a
   PR that adds the TablePro remote + manifest per Flathub docs.
2. Run `flatpak-builder --user --install build-dir flatpak/com.tablepro.linux.json`.
3. AppStream validate: `appstreamcli validate flatpak/com.tablepro.linux.metainfo.xml`.
4. Attach screenshots and a short release blurb from `CHANGELOG.md`.

Runtime permissions stay minimal: network, home (saved connections /
SSH keys), and talk to `org.freedesktop.secrets` for passwords and MCP
tokens.
