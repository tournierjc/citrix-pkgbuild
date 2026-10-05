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

## NVIDIA decode

Workspace app for Linux turns off hardware decode when it sees an NVIDIA GPU.
The package patches that check and points VAAPI at `libva-nvidia-driver`
(`LIBVA_DRIVER_NAME=nvidia`) so NVDEC can handle H.264/H.265. Session size is
left to the ICA file and the Desktop Viewer (windowed or multi-monitor).

VAAPI tops out around 4096×4096, so a desktop spanning both 4K screens may
still fall back to CPU H.264. After a session starts,
`journalctl --user -t citrix-wfica` should show VAAPI/hardware decode rather
than `Hardware decoding disabled because Nvidia GPU is installed`.

To temporarily restore vendor behaviour, unset `LIBVA_DRIVER_NAME` and replace
`/opt/Citrix/ICAClient/wfica.real` from a stock RPM.

## Keyboard (nested RDP)

Packaged `wfclient.ini` uses:

- `KeyboardLayout=French`
- `KeyboardEventMode=Scancode` (needed for Ctrl+C / Ctrl+V inside MSTSC)
- `KeyboardSyncMode=Once`
- `UseEUKS=2`, `UseEUKSforASCII=True`, `KeyboardSendLocale=True`
- `MouseSendsControlV=False`

Those keyboard values are also locked in `All_Regions.ini`, so a StoreFront ICA
file cannot switch the session to US QWERTY (nested MSTSC / Winlogon).

The Windows VDA logon screen often stays US QWERTY until the user profile
loads. `UseEUKSforASCII` sends letters as Unicode so AZERTY still types on
that screen. For a permanent logon-layout fix, copy the French layout to the
Windows welcome screen (Settings → Time & language → Administrative language
settings).

## Clipboard (Wayland ↔ Citrix)

Plasma copies screenshots as Wayland `image/png`. `wfica` only imports X11
`DIB` / `_ISL_DIB` / `PIXMAP`. The daemon
`/opt/Citrix/ICAClient/util/clip-bridge-rs` (Rust; the Python
`citrix-clip-bridge` is still installed as a fallback):

- watches Wayland `image/png` (`wl-paste --watch`) and caches a Citrix DIB
- while a Citrix window is focused, owns X11 `CLIPBOARD` and offers
  `_ISL_DIB` (32bpp, pixels at offset `0x428`, one-shot `XChangeProperty`
  with BIG-REQUESTS)
- does not take `PRIMARY` (Linux middle-click)
- when you copy in the session, reads the text (or `_ISL_DIB` image) straight
  from `wfica`, keeps the Wayland clipboard empty while Citrix stays focused,
  and publishes the copy with `wl-copy` once focus leaves Citrix

Linux → Citrix text needs no bridge: KWin pushes the Wayland clipboard onto
X11 whenever a Citrix window gains focus. The empty-while-focused rule works
around a KWin race: on each new session copy `wfica` drops X11 `CLIPBOARD`
for a moment before re-taking it, KWin fills that gap with its stale copy of
the previous clipboard, and `wfica` then replaces the session clipboard with
it (a Ctrl+C in Citrix seemed to need pressing twice).
- downscales images above the X11 BIG-REQUESTS limit (~16 MB, e.g. a
  full-screen 4K capture becomes ~2730x1535) so the paste still works;
  wfica cannot reassemble INCR chunks, so chunking is not an option

It is a **user systemd service** and starts with the graphical session, but
it stays idle until a `wfica` process is running. Copy/paste on the host is
left alone when Citrix is closed.

```bash
systemctl --user status citrix-clip-bridge.service
journalctl --user -t citrix-clip-bridge-rs -f
```

The unit is enabled by the package
(`graphical-session.target.wants`). After a reboot it comes up with Plasma;
bridging begins when you launch Citrix. `Restart=on-failure` brings it back
if it crashes.

The `wfica` wrapper starts the same binary only if the systemd unit is not
already active (singleton `flock` in `$XDG_RUNTIME_DIR`).

## License

The Citrix client remains under Citrix’s license. This repository only
contains packaging, the clip bridge, and keyboard defaults.
