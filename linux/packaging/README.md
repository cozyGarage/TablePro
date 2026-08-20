# Packaging

The first release target is an internal Arch Linux release candidate. Nothing
in this directory is ready for AUR or Flathub publication; the product name,
repository, application ID, portal permissions, and public update channel are
still provisional.

## Internal Arch RC

The recipe in [`arch/PKGBUILD`](arch/PKGBUILD) accepts only an immutable commit
archive and a real SHA-256 checksum. It installs `tablepro`,
`tablepro-agentd`, desktop/AppStream/icon metadata, the license, the example
policy, and any compiled translation catalogs. It deliberately does not ship a
systemd unit: stdio agentd is launched on demand by its MCP client.

After the exact commit passes the release gates and is tagged locally as
`linux-v0.1.0-rc1`:

```bash
./scripts/build-arch-rc.sh
```

The helper refuses a dirty tree or a local/remote tag that does not identify
`HEAD`, resolves the fork tag to a commit, downloads that immutable commit
archive, calculates its checksum, runs `makepkg --cleanbuild`, and runs
`namcap` plus package-content validation. Do not publish the resulting
artifact to the AUR.

Also validate the installed artifact in a clean Arch VM or container:

```bash
desktop-file-validate /usr/share/applications/com.tablepro.linux.desktop
appstreamcli validate --no-net /usr/share/metainfo/com.tablepro.linux.metainfo.xml
dbus-run-session -- tablepro
tablepro-agentd --help
```

Test install, upgrade, downgrade, and removal. Package operations must leave
the user's XDG configuration, data, state, and keyring records untouched.

## Debian / Ubuntu development package

The Debian files are a secondary development scaffold, not a release target.
For a local binary package:

```bash
./scripts/preflight.sh
./scripts/build-deb.sh
sudo apt install ./packaging/out/tablepro_*.deb
```

The package installs the GUI as `/usr/bin/tablepro` and agentd as an on-demand
`/usr/bin/tablepro-agentd` CLI. It does not install or enable a user service.

## Flatpak development build

The Flatpak manifest remains a non-release CI build:

```bash
./scripts/build-flatpak.sh
```

It still needs offline Cargo sources, a finalized identity, portal work, and a
filesystem-permission review before any Flathub submission. A successful
manifest build is not release evidence for the Arch RC.
