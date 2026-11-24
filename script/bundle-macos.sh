#!/usr/bin/env bash
# Crea target/release/bundle/kuaky.app (y el ícono .icns si falta).
# Uso: script/bundle-macos.sh [--install]   (--install copia la app a ~/Applications)
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
APP=target/release/bundle/kuaky.app
ICNS=assets/icon/kuaky.icns

if [[ ! -f $ICNS || assets/icon/kuaky.svg -nt $ICNS ]]; then
  echo "→ Generando ícono"
  TMP=$(mktemp -d)
  qlmanage -t -s 1024 -o "$TMP" assets/icon/kuaky.svg >/dev/null 2>&1
  SET="$TMP/kuaky.iconset"; mkdir "$SET"
  for s in 16 32 128 256 512; do
    sips -z $s $s "$TMP/kuaky.svg.png" --out "$SET/icon_${s}x${s}.png" >/dev/null
    sips -z $((s*2)) $((s*2)) "$TMP/kuaky.svg.png" --out "$SET/icon_${s}x${s}@2x.png" >/dev/null
  done
  iconutil -c icns "$SET" -o "$ICNS"
  rm -rf "$TMP"
fi

echo "→ Compilando (release)"
cargo build --release -p kuaky

echo "→ Armando $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/kuaky "$APP/Contents/MacOS/kuaky"
cp "$ICNS" "$APP/Contents/Resources/kuaky.icns"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>kuaky</string>
  <key>CFBundleDisplayName</key><string>kuaky</string>
  <key>CFBundleIdentifier</key><string>dev.kuaky.app</string>
  <key>CFBundleExecutable</key><string>kuaky</string>
  <key>CFBundleIconFile</key><string>kuaky</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

# Firma local (ad-hoc): suficiente para abrirla en esta Mac.
codesign --force --deep --sign - "$APP" >/dev/null
echo "✓ $APP"

if [[ "${1:-}" == "--install" ]]; then
  mkdir -p "$HOME/Applications"
  rm -rf "$HOME/Applications/kuaky.app"
  cp -R "$APP" "$HOME/Applications/kuaky.app"
  echo "✓ Instalada en ~/Applications/kuaky.app"
fi
