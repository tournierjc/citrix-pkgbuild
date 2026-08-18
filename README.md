# icaclient-bin

Arch / CachyOS package that repackages the official Citrix Workspace
`ICAClient` RHEL RPM. The wrapper keeps a French AZERTY scancode layout for
nested MSTSC, and a clip daemon bridges screenshot images between Plasma
Wayland and `wfica` (X11).

Upstream client: [Citrix Workspace app for Linux](https://www.citrix.com/downloads/workspace-app/linux/workspace-app-for-linux-latest.html).

## Build and install

```bash
cd citrix
makepkg -s
sudo pacman -U icaclient-bin-*.pkg.tar.zst
```

`makepkg` downloads the vendor RPM when Citrix still publishes a token on the
download page. If that fails, drop `ICAClient-rhel-gcc-8-<pkgver>-0.x86_64.rpm`
into `citrix/` and run `makepkg` again.

The `wfica` wrapper always passes `-clientfile /opt/Citrix/ICAClient/config/wfclient.ini`,
so `~/.ICAClient/wfclient.ini` is ignored for keyboard settings.

## Keyboard (nested RDP)

Packaged `wfclient.ini` uses:

- `KeyboardLayout=French`
- `KeyboardEventMode=Scancode` (needed for Ctrl+C / Ctrl+V inside MSTSC)
- `KeyboardSyncMode=Once`
- `UseEUKS=2`, `UseEUKSforASCII=True`, `KeyboardSendLocale=True`
- `MouseSendsControlV=False`

The Windows VDA logon screen often stays US QWERTY until the user profile
loads. `UseEUKSforASCII` sends letters as Unicode so AZERTY still types on
that screen. For a permanent logon-layout fix, copy the French layout to the
Windows welcome screen (Settings → Time & language → Administrative language
settings).

## Image clipboard (Wayland ↔ Citrix)

Plasma copies screenshots as Wayland `image/png`. `wfica` only imports X11
`DIB` / `_ISL_DIB` / `PIXMAP`. The daemon
`/opt/Citrix/ICAClient/util/citrix-clip-bridge`:

- watches Wayland `image/png` (`wl-paste --watch`)
- offers Citrix `_ISL_DIB` (32bpp, pixels at offset `0x428`, one-shot
  `XChangeProperty` with BIG-REQUESTS so GTK INCR cannot split the bitmap)
- pulls `_ISL_DIB` from `wfica` when you copy in the session and publishes
  `image/png` with `wl-copy`

It is a **user systemd service** and starts with the graphical session:

```bash
systemctl --user status citrix-clip-bridge.service
journalctl --user -t citrix-clip-bridge -f
```

The unit is enabled by the package
(`graphical-session.target.wants`). After a reboot it comes up with Plasma;
you do not need to launch Citrix first. `Restart=on-failure` brings it back
if it crashes.

The `wfica` wrapper starts the same binary only if the systemd unit is not
already active (singleton `flock` in `$XDG_RUNTIME_DIR`).

## License

The Citrix client remains under Citrix’s license. This repository only
contains packaging, the clip bridge, and keyboard defaults.
