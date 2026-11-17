#!/usr/bin/env python3
"""Ed25519 signing for SHA256SUMS.txt, using only Python's standard library and the `openssl` program (3.0+).

  python scripts/sign_sums.py keygen                        print a new seed (SECRET) and its public key
  UPDATE_SIGNING_SEED=<hex> python scripts/sign_sums.py sign FILE
                                                            write FILE.sig and print the public key
  python scripts/sign_sums.py verify FILE PUBLIC_KEY_HEX    exit 0 only if FILE.sig matches FILE and the key

Keys are 32 bytes written as 64 hex characters. The signature is 64 bytes written as 128 hex characters plus a
newline. The signature covers the exact bytes of FILE, so the server-side updater can check the checksum list
before it trusts any checksum in it. The seed is read from the environment, never from the command line, so it
cannot end up in a process list or a log.
"""

from __future__ import annotations

import base64
import os
import subprocess
import sys
import tempfile
from pathlib import Path

# DER headers that turn a raw 32-byte Ed25519 key into the PKCS#8 / SubjectPublicKeyInfo form OpenSSL reads.
_PKCS8_PREFIX = bytes.fromhex("302e020100300506032b657004220420")
_SPKI_PREFIX = bytes.fromhex("302a300506032b6570032100")


def _hex32(text: str, what: str) -> bytes:
    text = text.strip()
    try:
        raw = bytes.fromhex(text)
    except ValueError as exc:
        raise ValueError(f"{what} must be hexadecimal") from exc
    if len(raw) != 32:
        raise ValueError(f"{what} must be 32 bytes (64 hex characters), got {len(raw)} bytes")
    return raw


def _pem(label: str, der: bytes) -> str:
    body = base64.encodebytes(der).decode().strip()
    return f"-----BEGIN {label}-----\n{body}\n-----END {label}-----\n"


def _openssl(*args: str) -> bytes:
    try:
        done = subprocess.run(["openssl", *args], check=True, capture_output=True)
    except FileNotFoundError as exc:
        raise RuntimeError("the `openssl` program is not installed") from exc
    except subprocess.CalledProcessError as exc:
        detail = exc.stderr.decode("utf-8", "replace").strip()[:300]
        raise RuntimeError(f"openssl {args[0]} failed: {detail}") from exc
    return done.stdout


def public_key_of(seed: bytes) -> bytes:
    with tempfile.TemporaryDirectory() as tmp:
        key = Path(tmp, "key.pem")
        key.write_text(_pem("PRIVATE KEY", _PKCS8_PREFIX + seed), encoding="ascii")
        key.chmod(0o600)
        der = _openssl("pkey", "-in", str(key), "-pubout", "-outform", "DER")
    if len(der) != len(_SPKI_PREFIX) + 32 or not der.startswith(_SPKI_PREFIX):
        raise RuntimeError("openssl returned an unexpected public key format")
    return der[len(_SPKI_PREFIX):]


def sign(seed: bytes, data_file: Path) -> bytes:
    if data_file.stat().st_size == 0:
        raise ValueError(f"refusing to sign {data_file}: it is empty")
    with tempfile.TemporaryDirectory() as tmp:
        key = Path(tmp, "key.pem")
        key.write_text(_pem("PRIVATE KEY", _PKCS8_PREFIX + seed), encoding="ascii")
        key.chmod(0o600)
        signature = Path(tmp, "sig.bin")
        _openssl("pkeyutl", "-sign", "-inkey", str(key), "-rawin", "-in", str(data_file), "-out", str(signature))
        return signature.read_bytes()


def verify(public_key: bytes, data_file: Path, signature: bytes) -> bool:
    with tempfile.TemporaryDirectory() as tmp:
        pub = Path(tmp, "pub.pem")
        pub.write_text(_pem("PUBLIC KEY", _SPKI_PREFIX + public_key), encoding="ascii")
        sig = Path(tmp, "sig.bin")
        sig.write_bytes(signature)
        try:
            _openssl("pkeyutl", "-verify", "-pubin", "-inkey", str(pub), "-rawin", "-in", str(data_file), "-sigfile", str(sig))
        except RuntimeError:
            return False
    return True


def main(argv: list[str]) -> int:
    try:
        if argv[1:2] == ["keygen"] and len(argv) == 2:
            seed = os.urandom(32)
            print(f"seed (SECRET, store as the UPDATE_SIGNING_SEED secret): {seed.hex()}")
            print(f"public key (store as the UPDATE_PUBLIC_KEY variable):   {public_key_of(seed).hex()}")
            return 0
        if argv[1:2] == ["sign"] and len(argv) == 3:
            seed = _hex32(os.environ.get("UPDATE_SIGNING_SEED", ""), "UPDATE_SIGNING_SEED")
            target = Path(argv[2])
            signature = sign(seed, target)
            Path(f"{target}.sig").write_text(signature.hex() + "\n", encoding="ascii")
            print(f"signed {target} -> {target}.sig (public key {public_key_of(seed).hex()})")
            return 0
        if argv[1:2] == ["verify"] and len(argv) == 4:
            target = Path(argv[2])
            public_key = _hex32(argv[3], "public key")
            signature = bytes.fromhex(Path(f"{target}.sig").read_text(encoding="ascii").strip())
            if verify(public_key, target, signature):
                print(f"ok: {target}.sig is a valid signature of {target}")
                return 0
            print(f"FAIL: {target}.sig does not match {target} and this key", file=sys.stderr)
            return 1
    except (ValueError, RuntimeError, OSError) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1
    print(__doc__)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
