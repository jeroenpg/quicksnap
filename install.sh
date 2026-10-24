#!/usr/bin/env bash
# Install quicksnap for the current user, then (re)start it.
# Uses the prebuilt ./quicksnap from a release tarball if present, else builds from source.
set -euo pipefail
cd "$(dirname "$0")"

BIN="$HOME/.local/bin/quicksnap"
APPS="$HOME/.local/share/applications"
UNIT_DIR="$HOME/.config/systemd/user"

if [ -x ./quicksnap ] && [ ! -f Cargo.toml ]; then
    SRC=./quicksnap
else
    cargo build --release
    SRC=target/release/quicksnap
fi

# Atomic replace so a running daemon's binary is never overwritten in place.
mkdir -p "$(dirname "$BIN")"
install -m755 "$SRC" "$BIN.new"
mv -f "$BIN.new" "$BIN"

# KWin only hands out screenshots to binaries whose desktop file asks for it.
mkdir -p "$APPS"
cat > "$APPS/quicksnap.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=QuickSnap
Comment=Instant region screenshot
Exec=$BIN
Icon=applets-screenshooter
Categories=Utility;
X-KDE-DBUS-Restricted-Interfaces=org.kde.KWin.ScreenShot2
EOF
kbuildsycoca6 >/dev/null 2>&1 || true

mkdir -p "$UNIT_DIR"
cat > "$UNIT_DIR/quicksnap.service" <<EOF
[Unit]
Description=QuickSnap screenshot daemon
PartOf=graphical-session.target
After=graphical-session.target
StartLimitIntervalSec=0

[Service]
ExecStart=$BIN
Restart=on-failure
RestartSec=1

[Install]
WantedBy=graphical-session.target
EOF

# Free Print from Spectacle's region capture (it keeps Meta+Shift+Print).
KGA=(busctl --user call org.kde.kglobalaccel /kglobalaccel org.kde.KGlobalAccel)
"${KGA[@]}" setShortcut asaiu 4 org.kde.spectacle.desktop RectangularRegionScreenShot Spectacle \
    "Capture Rectangular Region" 1 301989897 4 >/dev/null || true
"${KGA[@]}" setShortcut asaiu 4 org.kde.spectacle.desktop _launch Spectacle "Launch Spectacle" \
    1 301989971 4 >/dev/null || true
kwriteconfig6 --file kglobalshortcutsrc --group org.kde.spectacle.desktop \
    --key RectangularRegionScreenShot "Meta+Shift+Print,Meta+Shift+Print,Capture Rectangular Region"
kwriteconfig6 --file kglobalshortcutsrc --group org.kde.spectacle.desktop \
    --key _launch "Meta+Shift+S,Meta+Shift+S,Launch Spectacle"

systemctl --user daemon-reload
systemctl --user enable quicksnap.service >/dev/null 2>&1
systemctl --user restart quicksnap.service
sleep 1
systemctl --user --no-pager status quicksnap.service | head -n 3
journalctl --user -u quicksnap.service -n 5 --no-pager -o cat
