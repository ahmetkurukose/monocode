#!/usr/bin/env bash
# Package an already-built native executable; do not build or launch the app.
# Development bundles use separate data; release bundles use installed app data.
set -euo pipefail
cd "$(dirname "$0")/.."
if [ "$(uname -s)" != Darwin ]; then
  echo 'macOS app bundles must be packaged on macOS.' >&2
  exit 1
fi

native_profile="${1:-release}"
native_target="${2:-${CARGO_TARGET_DIR:-crates/monocode-gpui/target}}/$native_profile"
native_version="${3:-$(sed -n 's/^version = "\([^"]*\)".*/\1/p' crates/monocode-gpui/Cargo.toml)}"
if [ "$native_profile" = release ]; then
  native_data_mode="${4:-monocode-data}"
else
  native_data_mode="${4:-dev-data}"
fi
case "$native_data_mode" in
  dev-data|monocode-data) ;;
  *) echo "Unknown data mode: $native_data_mode" >&2; exit 1 ;;
esac
# The name people see (Finder, Dock, menu bar) and the bundle's file name. The
# bundle identifier and the executable never change with it, so a renamed
# build keeps the same data, keychain items and website sign-ins.
native_name="${MONOCODE_NATIVE_APP_NAME:-Monocode Native}"
native_bundle="$native_target/$native_name.app"
native_binary="$native_target/monocode-gpui"
native_helper="$native_target/monocode-cef-helper"
native_pty_exec="$native_target/monocode-pty-exec"
for native_built in "$native_binary" "$native_helper" "$native_pty_exec"; do
  if [ ! -f "$native_built" ]; then
    echo "Missing native executable: $native_built" >&2
    echo 'Build it with cargo first (-p monocode-gpui -p monocode-cef -p monocode-core), or use npm run native:build.' >&2
    exit 1
  fi
done

mkdir -p "$native_bundle/Contents/MacOS" "$native_bundle/Contents/Resources"
# Replace the inode atomically so macOS does not reuse an older signature cache.
cp "$native_binary" "$native_bundle/Contents/MacOS/monocode-gpui.next"
mv -f "$native_bundle/Contents/MacOS/monocode-gpui.next" "$native_bundle/Contents/MacOS/monocode-gpui"
# Starts terminal children without fork (crates/monocode-core/src/process/pty.rs),
# found beside the app.
cp "$native_pty_exec" "$native_bundle/Contents/MacOS/monocode-pty-exec"
cp src-tauri/icons/icon.icns "$native_bundle/Contents/Resources/monocode.icns"
# macOS 26 draws the Liquid Glass icon from Assets.car; the .icns covers older releases.
cp src-tauri/macos/Assets.car "$native_bundle/Contents/Resources/Assets.car"
# Ship the app's attribution and the license texts for GPUI and embedded assets.
# Add these before signing so the signature covers the notices too.
native_licenses="$native_bundle/Contents/Resources/licenses"
mkdir -p "$native_licenses"
cp LICENSE NOTICE docs/native/LICENSE-GPUI-APACHE docs/native/LICENSE-MERMAID-MIT "$native_licenses/"
cp crates/monocode-gpui/assets/hugeicons.LICENSE \
  crates/monocode-gpui/assets/dark-plus.LICENSE \
  crates/monocode-gpui/assets/native-editor.LICENSE.txt "$native_licenses/"
cp crates/monocode-gpui/assets/material/LICENSE "$native_licenses/material-icons.LICENSE"
rm -f "$native_licenses/Geist-OFL.txt"
# Launch the binary itself: an exec'ing launcher changes the pid version, and macOS
# then refuses the menu bar item. The app reads MonocodeDataMode from Info.plist.
native_executable="monocode-gpui"
native_short_version="${native_version%%-*}"
native_build_version="$native_short_version"
if [[ "$native_version" == *-preview.* ]]; then
  native_build_version="${native_version##*-preview.}"
fi
rm -f "$native_bundle/Contents/MacOS/monocode-launcher"
cat > "$native_bundle/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>$native_executable</string>
<key>MonocodeDataMode</key><string>$native_data_mode</string>
<key>CFBundleIdentifier</key><string>dev.usemono.gpui</string>
<key>CFBundleName</key><string>$native_name</string>
<key>CFBundleDisplayName</key><string>$native_name</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>$native_short_version</string>
<key>CFBundleVersion</key><string>$native_build_version</string>
<key>MonocodeNativeVersion</key><string>$native_version</string>
<key>CFBundleIconFile</key><string>monocode.icns</string>
<key>CFBundleIconName</key><string>AppIcon</string>
<key>NSPrincipalClass</key><string>NSApplication</string>
<key>NSHighResolutionCapable</key><true/>
<key>NSBluetoothAlwaysUsageDescription</key><string>Websites in the browser can use Bluetooth, for example to sign in with a passkey on your phone.</string>
<key>NSBluetoothPeripheralUsageDescription</key><string>Websites in the browser can use Bluetooth, for example to sign in with a passkey on your phone.</string>
<key>NSCameraUsageDescription</key><string>Websites in the browser can ask to use your camera.</string>
<key>NSMicrophoneUsageDescription</key><string>Websites in the browser can ask to use your microphone.</string>
<key>NSLocationUsageDescription</key><string>Websites in the browser can ask for your location.</string>
<key>NSLocationWhenInUseUsageDescription</key><string>Websites in the browser can ask for your location.</string>
<key>CFBundleURLTypes</key><array><dict>
<key>CFBundleURLName</key><string>Web site URL</string>
<key>CFBundleTypeRole</key><string>Viewer</string>
<key>CFBundleURLSchemes</key><array><string>http</string><string>https</string></array>
</dict></array>
</dict></plist>
PLIST
# The signing identity: MONOCODE_NATIVE_SIGNING_IDENTITY (a Developer ID for
# releases, "-" for ad hoc), else the stable local certificate
# scripts/native-dev-signing-cert.sh makes, when this Mac has it (K59).
native_identity="${MONOCODE_NATIVE_SIGNING_IDENTITY:-}"
native_local_name="${MONOCODE_NATIVE_LOCAL_IDENTITY:-Monocode Local Signing}"
if [ -z "$native_identity" ]; then
  native_local_hash="$(security find-identity -v -p codesigning 2>/dev/null \
    | awk -v name="\"$native_local_name\"" 'index($0, name) { print $2; exit }')"
  if [ -n "$native_local_hash" ]; then
    native_identity="$native_local_hash"
    MONOCODE_NATIVE_LOCAL_SIGNING=1
    printf 'Signing with "%s" (%s).\n' "$native_local_name" "$native_local_hash"
  else
    native_identity=-
  fi
fi
if [ "$native_identity" = - ]; then
  # An ad-hoc signature is the code's hash: macOS ties what the user allowed
  # to it, so every build starts over.
  echo 'warning: signing ad hoc. macOS forgets the Bluetooth, camera and microphone permissions and the keychain'"'"'s "Always Allow" with every build.' >&2
  echo 'warning: run scripts/native-dev-signing-cert.sh once to sign every build with the same local certificate.' >&2
fi
# Signs one bundle. Ad-hoc and local signatures have no hardened runtime, so
# they need no entitlements to load the downloaded Chromium framework.
native_sign() {
  local entitlements="$1" bundle="$2"
  if [ "$native_identity" = - ]; then
    codesign --force --sign - "$bundle"
  elif [ -n "${MONOCODE_NATIVE_LOCAL_SIGNING:-}" ]; then
    # A certificate made on this Mac (not notarized): the signature stays the
    # same from build to build, so the keychain's "Always Allow" survives updates.
    codesign --force --sign "$native_identity" "$bundle"
  else
    codesign --force --sign "$native_identity" --options runtime --timestamp \
      --entitlements "$entitlements" "$bundle"
  fi
}

# Signs code that carries no entitlements: libraries and frameworks.
native_sign_code() {
  if [ "$native_identity" = - ]; then
    codesign --force --sign - "$1"
  elif [ -n "${MONOCODE_NATIVE_LOCAL_SIGNING:-}" ]; then
    codesign --force --sign "$native_identity" "$1"
  else
    codesign --force --sign "$native_identity" --timestamp "$1"
  fi
}

native_frameworks="$native_bundle/Contents/Frameworks"
mkdir -p "$native_frameworks"

# The Chromium framework (docs/native/browser/context.md §4.3). It must be in
# the bundle: Chromium's macOS sandbox lets the helpers read only inside the
# app. It comes from the CEF SDK cef-dll-sys fetched for this build, so it is
# the exact build the app was compiled against.
native_cef="${MONOCODE_CEF_FRAMEWORK:-}"
if [ -z "$native_cef" ]; then
  native_cef_version="$(sed -n '/^name = "cef-dll-sys"$/{n;s/^version = ".*+\(.*\)"$/\1/p;}' crates/monocode-gpui/Cargo.lock)"
  case "$(lipo -archs "$native_binary" 2>/dev/null || uname -m)" in
    arm64) native_cef_arch=aarch64 ;;
    x86_64) native_cef_arch=x86_64 ;;
    *) echo "Unknown architecture of $native_binary" >&2; exit 1 ;;
  esac
  native_cef="${CEF_PATH:-crates/monocode-gpui/target/cef-sdk}/$native_cef_version/cef_macos_$native_cef_arch/Chromium Embedded Framework.framework"
fi
if [ ! -f "$native_cef/Chromium Embedded Framework" ]; then
  echo "Missing the Chromium framework: $native_cef" >&2
  echo 'Build the app with cargo first; cef-dll-sys fetches it (see .cargo/config.toml).' >&2
  exit 1
fi
# ~250 MB: copied and signed again only when it or the signing identity changed.
native_cef_dest="$native_frameworks/Chromium Embedded Framework.framework"
native_cef_stamp="$native_target/.cef-framework-$native_name.stamp"
native_cef_state="$native_identity|$(stat -f '%z %m' "$native_cef/Chromium Embedded Framework")"
if [ ! -d "$native_cef_dest" ] || [ "$(cat "$native_cef_stamp" 2>/dev/null)" != "$native_cef_state" ]; then
  rm -rf "$native_cef_dest"
  ditto "$native_cef" "$native_cef_dest"
  # Inside out: its libraries, then the framework.
  for native_cef_library in "$native_cef_dest/Libraries/"*.dylib; do
    native_sign_code "$native_cef_library"
  done
  native_sign_code "$native_cef_dest"
  printf '%s' "$native_cef_state" > "$native_cef_stamp"
fi

# Chromium's child processes: one small executable in five helper bundles.
# Chromium finds the GPU, Renderer, Plugin and Alerts helpers by adding their
# suffix to the main helper's name, so all five carry the app's visible name.
find "$native_frameworks" -maxdepth 1 -name '* Helper*.app' -exec rm -rf {} +
for native_variant in "" " (GPU)" " (Renderer)" " (Plugin)" " (Alerts)"; do
  native_helper_name="$native_name Helper$native_variant"
  native_helper_app="$native_frameworks/$native_helper_name.app"
  native_helper_id="dev.usemono.gpui.helper"
  case "$native_variant" in
    " (GPU)") native_helper_id="$native_helper_id.gpu" ;;
    " (Renderer)") native_helper_id="$native_helper_id.renderer" ;;
    " (Plugin)") native_helper_id="$native_helper_id.plugin" ;;
    " (Alerts)") native_helper_id="$native_helper_id.alerts" ;;
  esac
  mkdir -p "$native_helper_app/Contents/MacOS"
  cp "$native_helper" "$native_helper_app/Contents/MacOS/$native_helper_name"
  cat > "$native_helper_app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>$native_helper_name</string>
<key>CFBundleIdentifier</key><string>$native_helper_id</string>
<key>CFBundleName</key><string>$native_helper_name</string>
<key>CFBundleDisplayName</key><string>$native_helper_name</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>$native_short_version</string>
<key>CFBundleVersion</key><string>$native_build_version</string>
<key>LSUIElement</key><true/>
<key>LSFileQuarantineEnabled</key><true/>
<key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict></plist>
PLIST
  native_sign crates/monocode-cef/macos/Helper.entitlements "$native_helper_app"
done
# Nested code first: the app's signature seals it.
native_sign crates/monocode-core/macos/PtyExec.entitlements "$native_bundle/Contents/MacOS/monocode-pty-exec"
native_sign crates/monocode-gpui/macos/Entitlements.plist "$native_bundle"
printf 'App: %s\n' "$native_bundle"
