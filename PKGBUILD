pkgname=icaclient-bin
pkgver=26.01.0.150
pkgrel=9
pkgdesc="Citrix ICA Client (repackaged vendor RPM, no compilation)"
arch=('x86_64')
url="https://www.citrix.com"
license=('custom:Citrix')
depends=(
  bash bubblewrap
  libxss libxrandr libxcursor libxcomposite libxdamage libxfixes libxinerama libxtst
  alsa-lib libx11 libxcb nss gtk3 libsoup webkit2gtk-4.1
)
optdepends=(
  'lshw: hardware probing some Citrix diagnostics may attempt to use'
)
provides=('icaclient')
conflicts=('icaclient')

# Dynamically discover the vendor RPM download URL from Citrix's workspace page,
# mirroring the AUR icaclient PKGBUILD approach. This lets makepkg download the
# RPM automatically rather than requiring a manual drop-in.
_citrix_dl_page="https://www.citrix.com/downloads/workspace-app/linux/workspace-app-for-linux-latest.html"
_dl_urls="$(curl -sL "$_citrix_dl_page" | grep -oP '(?<=href=")[^"]+linuxx64[^"]+\.rpm(\?[^"]*)?' | head -n1 || true)"

if [[ -n "$_dl_urls" ]]; then
  source=("ICAClient-rhel-${pkgver}-0.x86_64.rpm::https:${_dl_urls}")
else
  source=("ICAClient-rhel-${pkgver}-0.x86_64.rpm")
fi
noextract=("ICAClient-rhel-${pkgver}-0.x86_64.rpm")
sha256sums=('SKIP')
install=icaclient-bin.install
options=(!strip)

build() {
  cd "$srcdir"
  rm -rf opt usr etc
  rpm2cpio "ICAClient-rhel-${pkgver}-0.x86_64.rpm" | cpio -idmv
}

package() {
  cd "$srcdir"
  source=("ICAClient-${pkgver}.rpm::${_source64}")
  noextract=("ICAClient-${pkgver}.rpm")
  sha256sums=('SKIP')
else
  # Fallback to the old behaviour: expect a local RPM file with the exact name.
  source=("ICAClient-rhel-${pkgver}-0.x86_64.rpm")
  noextract=("ICAClient-rhel-${pkgver}-0.x86_64.rpm")
  sha256sums=('SKIP')
fi

install=icaclient-bin.install
options=(!strip)

build() {
  cd "$srcdir"
  rm -rf opt usr etc
  # Support both dynamically downloaded name and legacy RPM name.
  if [[ -f "ICAClient-${pkgver}.rpm" ]]; then
    rpm2cpio "ICAClient-${pkgver}.rpm" | cpio -idmv
  elif [[ -f "ICAClient-rhel-${pkgver}-0.x86_64.rpm" ]]; then
    rpm2cpio "ICAClient-rhel-${pkgver}-0.x86_64.rpm" | cpio -idmv
  else
    # fallback: use first .rpm in srcdir
    set -- *.rpm
    if [[ -f "$1" ]]; then
      rpm2cpio "$1" | cpio -idmv
    else
      echo "No RPM found to extract"; return 1
    fi
  fi
}

package() {
  cd "$srcdir"

  for d in opt usr etc; do
    [[ -d "$d" ]] && cp -a "$d" "$pkgdir/"
  done

  local root="$pkgdir/opt/Citrix/ICAClient"
  [[ -d "$root" ]] || { echo "Missing $root after RPM extraction"; return 1; }

  install -d "$root/config"

  # Normalize the bundled WebKit payload into a usable private libdir.
  # The vendor tarball expands to webkit2gtk-4.0-package/usr/lib/x86_64-linux-gnu/*,
  # which is not searched by our wrapper. Copy the relevant libs and helpers into
  # /opt/Citrix/ICAClient/Webkit2gtk4.0/lib.
  local wktar="$root/Webkit2gtk4.0/webkit2gtk-4.0.tar.gz"
  if [[ -f "$wktar" ]]; then
    local wkstage="$srcdir/webkit2gtk-stage"
    rm -rf "$wkstage"
    mkdir -p "$wkstage"
    tar -xzf "$wktar" -C "$wkstage"

    local wkdeb="$wkstage/webkit2gtk-4.0-package/usr/lib/x86_64-linux-gnu"
    if [[ -d "$wkdeb" ]]; then
      install -d "$root/Webkit2gtk4.0/lib"
      cp -a "$wkdeb"/lib*.so* "$root/Webkit2gtk4.0/lib/" 2>/dev/null || true
      if [[ -d "$wkdeb/webkit2gtk-4.0" ]]; then
        cp -a "$wkdeb/webkit2gtk-4.0" "$root/Webkit2gtk4.0/lib/"
      fi
    fi
  fi

  # Remove embedded Debian package tree so it does not conflict with prior manual extractions.
  rm -rf "$root/Webkit2gtk4.0/webkit2gtk-4.0-package"

  # Minimal config files required by the runtime.
  # Force a French default profile at package level.
  if [[ -f "$root/nls/fr/wfclient.template" ]]; then
    cp "$root/nls/fr/wfclient.template" "$root/config/wfclient.ini"
  elif [[ ! -f "$root/config/wfclient.ini" ]]; then
    cat > "$root/config/wfclient.ini" <<'EOF'
[WFClient]
Version=2
KeyboardLayout = French
KeyboardType=(Default)
KeyboardMappingFile = automatic.kbd
KeyboardDescription = French
KeyboardEventMode = Automatic
KeyboardSyncMode = Off
ClientAudio=On
AllowLocalDrives=True
DisablePrinterRedirection=False
EOF
  fi

  if [[ -f "$root/config/wfclient.ini" ]]; then
    sed -i \
      -e 's/^KeyboardLayout *=.*/KeyboardLayout = French/' \
      -e 's/^KeyboardType *=.*/KeyboardType=(Default)/' \
      -e 's/^KeyboardMappingFile *=.*/KeyboardMappingFile = automatic.kbd/' \
      -e 's/^KeyboardDescription *=.*/KeyboardDescription = French/' \
      -e 's/^KeyboardEventMode *=.*/KeyboardEventMode = Automatic/' \
      -e 's/^KeyboardSyncMode *=.*/KeyboardSyncMode = Off/' \
      "$root/config/wfclient.ini"
  fi

  # Force the same keyboard defaults in appsrv.ini as well; some Citrix flows
  # still consult [WFClient] there.
  if [[ -f "$root/config/appsrv.ini" ]]; then
    if ! grep -q '^\[WFClient\]' "$root/config/appsrv.ini"; then
      printf '\n[WFClient]\n' >> "$root/config/appsrv.ini"
    fi
    if grep -q '^KeyboardLayout *=.*' "$root/config/appsrv.ini"; then
      sed -i 's/^KeyboardLayout *=.*/KeyboardLayout=French/' "$root/config/appsrv.ini"
    else
      sed -i '/^\[WFClient\]/a KeyboardLayout=French' "$root/config/appsrv.ini"
    fi
  else
    cat > "$root/config/appsrv.ini" <<'EOF'
[WFClient]
KeyboardLayout=French
EOF
  fi

  if [[ ! -f "$root/config/module.ini" ]]; then
    if [[ -f "$root/nls/fr/module.ini" ]]; then
      cp "$root/nls/fr/module.ini" "$root/config/module.ini"
    elif [[ -f "$root/nls/en/module.ini" ]]; then
      cp "$root/nls/en/module.ini" "$root/config/module.ini"
    elif [[ -f "$root/nls/en_US/module.ini" ]]; then
      cp "$root/nls/en_US/module.ini" "$root/config/module.ini"
    fi
  fi

  # Some client startup paths validate ICAROOT by checking for top-level support
  # files such as eula.txt. The RPM only ships localized copies under nls/*.
  if [[ ! -f "$root/eula.txt" ]]; then
    if [[ -f "$root/nls/en.UTF-8/eula.txt" ]]; then
      ln -sf nls/en.UTF-8/eula.txt "$root/eula.txt"
    elif [[ -f "$root/nls/en/eula.txt" ]]; then
      ln -sf nls/en/eula.txt "$root/eula.txt"
    fi
  fi

  # Default permissions first, then restore executables.
  find "$root" -type d -exec chmod 755 {} +
  find "$root" -type f -exec chmod 644 {} +

  local execs=(
    wfica wfcrun32 icasessionmgr ctxfuse selfservice PrimaryAuthManager
    AuthManagerDaemon UtilDaemon NativeMessagingHost
  )
  for b in "${execs[@]}"; do
    [[ -f "$root/$b" ]] && chmod 755 "$root/$b"
  done
  find "$root" \( -name '*.so' -o -name '*.so.*' \) -type f -exec chmod 755 {} +
  [[ -d "$root/Webkit2gtk4.0/lib" ]] && find "$root/Webkit2gtk4.0/lib" -type f -exec chmod 755 {} +

  install -d "$pkgdir/usr/bin"

  # Match the vendor-generated launcher logic from integrate.sh:
  # set ICAROOT + LD_LIBRARY_PATH, then call wfica with -file for ICA files.
  cat > "$pkgdir/usr/bin/wfica" <<'EOF'
#!/bin/sh
ICAROOT=/opt/Citrix/ICAClient
export ICAROOT
export LD_LIBRARY_PATH="${ICAROOT}/lib:${ICAROOT}/Webkit2gtk4.0/lib${LD_LIBRARY_PATH:+:${LD_LIBRARY_PATH}}"
export GTK_PATH="${ICAROOT}/gtk${GTK_PATH:+:${GTK_PATH}}"
CLIENTFILE="${ICAROOT}/config/wfclient.ini"
PROTOCOLFILE="${ICAROOT}/config/module.ini"

if [ "$#" -eq 1 ] && [ -f "$1" ]; then
  case "$1" in
    *.ica|*.ICA)
      exec "$ICAROOT/wfica" -icaroot "$ICAROOT" -protocolfile "$PROTOCOLFILE" -clientfile "$CLIENTFILE" -file "$(readlink -f "$1")"
      ;;
  esac
fi

exec "$ICAROOT/wfica" -icaroot "$ICAROOT" -protocolfile "$PROTOCOLFILE" -clientfile "$CLIENTFILE" "$@"
EOF
  chmod 755 "$pkgdir/usr/bin/wfica"

  if [[ -f "$root/wfcrun32" ]]; then
    cat > "$pkgdir/usr/bin/wfcrun32" <<'EOF'
#!/bin/sh
ICAROOT=/opt/Citrix/ICAClient
export ICAROOT
export LD_LIBRARY_PATH="${ICAROOT}/lib:${ICAROOT}/Webkit2gtk4.0/lib${LD_LIBRARY_PATH:+:${LD_LIBRARY_PATH}}"
export GTK_PATH="${ICAROOT}/gtk${GTK_PATH:+:${GTK_PATH}}"
exec "$ICAROOT/wfcrun32" -icaroot "$ICAROOT" "$@"
EOF
    chmod 755 "$pkgdir/usr/bin/wfcrun32"
  fi

  install -d "$pkgdir/etc/profile.d"
  cat > "$pkgdir/etc/profile.d/icaclient.sh" <<'EOF'
export ICAROOT=/opt/Citrix/ICAClient
export LD_LIBRARY_PATH=/opt/Citrix/ICAClient/lib:/opt/Citrix/ICAClient/Webkit2gtk4.0/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}
export GTK_PATH=/opt/Citrix/ICAClient/gtk${GTK_PATH:+:$GTK_PATH}
EOF

  install -d "$pkgdir/etc/ld.so.conf.d"
  cat > "$pkgdir/etc/ld.so.conf.d/icaclient.conf" <<'EOF'
/opt/Citrix/ICAClient/lib
/opt/Citrix/ICAClient/Webkit2gtk4.0/lib
EOF

  install -d "$pkgdir/usr/share/applications"
  cat > "$pkgdir/usr/share/applications/citrix-ica.desktop" <<'EOF'
[Desktop Entry]
Name=Citrix ICA Client
Exec=/usr/bin/wfica %f
Type=Application
MimeType=application/x-ica;
Categories=Network;
EOF

  # Absolute-path compatibility shims expected by the proprietary binary.
  ln -sf /opt/Citrix/ICAClient "$pkgdir/ICAROOT"
  ln -sf /opt/Citrix/ICAClient/config "$pkgdir/config"
  ln -sf /opt/Citrix/ICAClient/gtk "$pkgdir/gtk"
  ln -sf /opt/Citrix/ICAClient/keyboard "$pkgdir/keyboard"
  ln -sf /opt/Citrix/ICAClient/keyboard "$pkgdir/keyboad"
  [[ -f "$root/CHARICONV.DLL" ]] && ln -sf /opt/Citrix/ICAClient/CHARICONV.DLL "$pkgdir/CHARICONV.DLL"

  install -d "$pkgdir/usr/lib"
  [[ -f "$root/lib/UIDialogLib3.so" ]] && \
    ln -sf /opt/Citrix/ICAClient/lib/UIDialogLib3.so "$pkgdir/usr/lib/UIDialogLib3.so"
  [[ -f "$root/lib/UIDialogLibWebKit3.so" ]] && \
    ln -sf /opt/Citrix/ICAClient/lib/UIDialogLibWebKit3.so "$pkgdir/usr/lib/UIDialogLibWebKit3.so"
}
