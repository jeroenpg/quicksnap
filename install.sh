#!/usr/bin/env bash
# Install quicksnap for the current user, then (re)start it.
# Uses the prebuilt ./quicksnap from a release tarball if present, else builds from source.
set -euo pipefail
cd "$(dirname "$0")"

BIN="$HOME/.local/bin/quicksnap"
APPS="$HOME/.local/share/applications"
UNIT_DIR="$HOME/.config/systemd/user"

case "${XDG_CURRENT_DESKTOP:-}" in
    *COSMIC*) DESKTOP=cosmic ;;
    *) DESKTOP=kde ;;
esac

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
if [ "$DESKTOP" = kde ]; then kbuildsycoca6 >/dev/null 2>&1 || true; fi

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

if [ "$DESKTOP" = kde ]; then
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
else
    # COSMIC has no kglobalaccel: bind Print in its custom shortcuts, which override the
    # defaults. cosmic-screenshot keeps Super+Shift+Print.
    SC_DIR="$HOME/.config/cosmic/com.system76.CosmicSettings.Shortcuts/v1"
    SC="$SC_DIR/custom"
    ENTRIES="    (modifiers: [], key: \"Print\", description: Some(\"QuickSnap\")): Spawn(\"$BIN capture\"),
    (modifiers: [Super, Shift], key: \"Print\"): System(Screenshot),"
    mkdir -p "$SC_DIR"
    if [ ! -s "$SC" ]; then
        printf '{\n%s\n}\n' "$ENTRIES" > "$SC"
    elif ! grep -q 'quicksnap capture' "$SC"; then
        cp "$SC" "$SC.bak-$(date +%Y%m%d-%H%M%S)"
        grep -q 'key: "Print"' "$SC" && echo "note: $SC already binds Print; check it in Settings"
        # Insert right after the map's opening brace.
        awk -v e="$ENTRIES" '!done && /\{/ { sub(/\{/, "{\n" e); done = 1 } 1' "$SC" > "$SC.new"
        mv -f "$SC.new" "$SC"
    fi
fi

systemctl --user daemon-reload
systemctl --user enable quicksnap.service >/dev/null 2>&1
systemctl --user restart quicksnap.service
sleep 1
systemctl --user --no-pager status quicksnap.service | head -n 3
journalctl --user -u quicksnap.service -n 5 --no-pager -o cat
