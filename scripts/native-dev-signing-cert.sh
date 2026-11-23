#!/usr/bin/env bash
# Creates, once, the local code-signing certificate native builds sign with
# (docs/native/browser/context.md K59), in your login keychain.
#
# Why: an ad-hoc signature is the build's code hash, which every build
# changes, so macOS forgets what you allowed the app (Bluetooth for signing
# in with your phone, camera, microphone) and the keychain's "Always Allow"
# with every build. A certificate of your own keeps one identity from build
# to build. scripts/bundle-native-macos.sh signs with it whenever it finds it
# (no Apple account needed; it is not for sharing the app with others).
#
# Usage: scripts/native-dev-signing-cert.sh [--keychain PATH]
# The name is "Monocode Local Signing" (MONOCODE_NATIVE_LOCAL_IDENTITY
# chooses another). Running it again changes nothing. To remove it: Keychain
# Access → login → My Certificates → "Monocode Local Signing" → Delete.
#
# The first build that signs with it may ask whether codesign may use the
# key: choose "Always Allow".
set -euo pipefail
if [ "$(uname -s)" != Darwin ]; then
  echo 'Code-signing certificates are for macOS.' >&2
  exit 1
fi

name="${MONOCODE_NATIVE_LOCAL_IDENTITY:-Monocode Local Signing}"
keychain="$HOME/Library/Keychains/login.keychain-db"
while [ $# -gt 0 ]; do
  case "$1" in
    --keychain) keychain="$2"; shift 2 ;;
    *) echo "Unknown option: $1" >&2; exit 1 ;;
  esac
done

if security find-identity -v -p codesigning "$keychain" 2>/dev/null | grep -Fq "\"$name\""; then
  echo "\"$name\" is already in $keychain; native builds sign with it."
  exit 0
fi
if security find-certificate -c "$name" "$keychain" >/dev/null 2>&1; then
  echo "\"$name\" is in $keychain but not trusted for code signing." >&2
  echo "Keychain Access → the certificate → Trust → Code Signing: Always Trust, or delete it and run this again." >&2
  exit 1
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
cat > "$work/req.cnf" <<CNF
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = $name
[ext]
basicConstraints = critical, CA:false
keyUsage = critical, digitalSignature
extendedKeyUsage = critical, codeSigning
subjectKeyIdentifier = hash
CNF
# macOS's own openssl (LibreSSL): its PKCS#12 files are the kind `security`
# imports; OpenSSL 3's default ciphers are not.
/usr/bin/openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -config "$work/req.cnf" \
  -keyout "$work/key.pem" -out "$work/cert.pem" 2>/dev/null
password="$(/usr/bin/openssl rand -hex 16)"
/usr/bin/openssl pkcs12 -export -inkey "$work/key.pem" -in "$work/cert.pem" \
  -name "$name" -passout "pass:$password" -out "$work/identity.p12"
# -T: codesign may use the key.
security import "$work/identity.p12" -k "$keychain" -P "$password" -T /usr/bin/codesign >/dev/null
# codesign takes only identities trusted for code signing. This is your
# account's trust setting for this one certificate; macOS asks for your
# password to change it.
echo "macOS now asks for your password to trust \"$name\" for code signing."
security add-trusted-cert -r trustRoot -p codeSign -k "$keychain" "$work/cert.pem"

if ! security find-identity -v -p codesigning "$keychain" | grep -Fq "\"$name\""; then
  echo "The certificate was imported, but macOS does not trust it for code signing." >&2
  echo "Keychain Access → the certificate → Trust → Code Signing: Always Trust." >&2
  exit 1
fi
echo "Created \"$name\" in $keychain."
echo 'Native builds (npm run native:dev, npm run native:build) now sign with it.'
echo 'macOS asks once more for Bluetooth, the camera and the microphone, then remembers.'
