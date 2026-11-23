#!/bin/bash
# Makes rsa-signed.crx: a CRX3 of a one-file zip, signed with a throwaway
# RSA-2048 key (crx.rs tests the RSA path with it). Needs openssl and python3.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
openssl genrsa -out "$work/key.pem" 2048 2>/dev/null
openssl rsa -in "$work/key.pem" -pubout -outform DER -out "$work/pub.der" 2>/dev/null
python3 -I - "$work" "$here/rsa-signed.crx" <<'PY'
import hashlib, io, struct, subprocess, sys, zipfile
work, out = sys.argv[1], sys.argv[2]
buf = io.BytesIO()
with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as z:
    z.writestr("manifest.json", '{"name":"t","version":"1","manifest_version":3}')
archive = buf.getvalue()
pub = open(f"{work}/pub.der", "rb").read()
def field(n, b):
    def varint(v):
        o = b""
        while True:
            c = v & 0x7F
            v >>= 7
            if v:
                o += bytes([c | 0x80])
            else:
                return o + bytes([c])
    return varint((n << 3) | 2) + varint(len(b)) + b
signed = field(1, hashlib.sha256(pub).digest()[:16])
message = b"CRX3 SignedData\x00" + struct.pack("<I", len(signed)) + signed + archive
open(f"{work}/msg", "wb").write(message)
sig = subprocess.run(["openssl", "dgst", "-sha256", "-sign", f"{work}/key.pem", f"{work}/msg"],
                     check=True, capture_output=True).stdout
header = field(2, field(1, pub) + field(2, sig)) + field(10000, signed)
open(out, "wb").write(b"Cr24" + struct.pack("<II", 3, len(header)) + header + archive)
PY
