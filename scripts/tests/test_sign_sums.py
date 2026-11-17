"""Ed25519 signing of the checksum list (scripts/sign_sums.py). Needs the `openssl` program (3.0+)."""

import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import sign_sums as ss  # noqa: E402

SCRIPT = Path(__file__).resolve().parent.parent / "sign_sums.py"
SEED = bytes(range(32))


def openssl_can_sign_raw() -> bool:
    if not shutil.which("openssl"):
        return False
    try:
        ss.public_key_of(SEED)
        return True
    except RuntimeError:
        return False


@unittest.skipUnless(openssl_can_sign_raw(), "needs OpenSSL 3 with Ed25519")
class RoundTrip(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.file = Path(self.tmp.name, "SHA256SUMS.txt")
        self.file.write_text("abc123  pyxeetb-0.0.1-x86_64-unknown-linux-musl.tar.gz\n", encoding="utf-8")

    def test_a_signature_verifies_with_the_matching_key(self):
        signature = ss.sign(SEED, self.file)
        self.assertEqual(len(signature), 64)
        self.assertTrue(ss.verify(ss.public_key_of(SEED), self.file, signature))

    def test_signing_is_deterministic(self):
        self.assertEqual(ss.sign(SEED, self.file), ss.sign(SEED, self.file))

    def test_a_changed_file_fails(self):
        signature = ss.sign(SEED, self.file)
        self.file.write_text(self.file.read_text(encoding="utf-8") + "extra\n", encoding="utf-8")
        self.assertFalse(ss.verify(ss.public_key_of(SEED), self.file, signature))

    def test_another_key_fails(self):
        signature = ss.sign(SEED, self.file)
        self.assertFalse(ss.verify(ss.public_key_of(bytes(reversed(range(32)))), self.file, signature))

    def test_a_known_public_key_for_a_known_seed(self):
        # RFC 8032 section 7.1, test vector 1.
        seed = bytes.fromhex("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60")
        self.assertEqual(ss.public_key_of(seed).hex(), "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a")

    def test_the_rfc_8032_vector_with_a_one_byte_message(self):
        # RFC 8032 section 7.1, test vector 2: the standard's own answer, so this agrees with every Ed25519 library.
        seed = bytes.fromhex("4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb")
        self.assertEqual(ss.public_key_of(seed).hex(), "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c")
        message = Path(self.tmp.name, "one-byte")
        message.write_bytes(bytes.fromhex("72"))
        expected = (
            "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da"
            "085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00"
        )
        self.assertEqual(ss.sign(seed, message).hex(), expected)

    def test_an_empty_file_is_refused(self):
        empty = Path(self.tmp.name, "empty")
        empty.write_bytes(b"")
        with self.assertRaises(ValueError):
            ss.sign(SEED, empty)

    def test_the_command_line_signs_and_verifies(self):
        env = {**os.environ, "UPDATE_SIGNING_SEED": SEED.hex()}
        done = subprocess.run([sys.executable, str(SCRIPT), "sign", str(self.file)], env=env, capture_output=True, encoding="utf-8")
        self.assertEqual(done.returncode, 0, done.stderr)
        sig_text = Path(f"{self.file}.sig").read_text(encoding="ascii")
        self.assertRegex(sig_text, r"^[0-9a-f]{128}\n$")
        self.assertNotIn(SEED.hex(), done.stdout + done.stderr, "the seed must never be printed")
        pub = ss.public_key_of(SEED).hex()
        ok = subprocess.run([sys.executable, str(SCRIPT), "verify", str(self.file), pub], capture_output=True, encoding="utf-8")
        self.assertEqual(ok.returncode, 0, ok.stderr)
        self.file.write_text("tampered\n", encoding="utf-8")
        bad = subprocess.run([sys.executable, str(SCRIPT), "verify", str(self.file), pub], capture_output=True, encoding="utf-8")
        self.assertEqual(bad.returncode, 1)

    def test_keygen_prints_a_usable_pair(self):
        done = subprocess.run([sys.executable, str(SCRIPT), "keygen"], capture_output=True, encoding="utf-8")
        self.assertEqual(done.returncode, 0, done.stderr)
        seed_hex = done.stdout.splitlines()[0].split()[-1]
        pub_hex = done.stdout.splitlines()[1].split()[-1]
        self.assertEqual(ss.public_key_of(bytes.fromhex(seed_hex)).hex(), pub_hex)


class Validation(unittest.TestCase):
    def test_seeds_and_keys_must_be_32_hex_bytes(self):
        for bad in ("", "zz", "ab" * 31, "ab" * 33):
            with self.assertRaises(ValueError):
                ss._hex32(bad, "key")
        self.assertEqual(ss._hex32("ab" * 32, "key"), bytes.fromhex("ab" * 32))

    def test_signing_without_a_seed_fails_clearly(self):
        env = {k: v for k, v in os.environ.items() if k != "UPDATE_SIGNING_SEED"}
        done = subprocess.run([sys.executable, str(SCRIPT), "sign", "nope.txt"], env=env, capture_output=True, encoding="utf-8")
        self.assertEqual(done.returncode, 1)
        self.assertIn("UPDATE_SIGNING_SEED", done.stderr)

    def test_unknown_usage_prints_help(self):
        done = subprocess.run([sys.executable, str(SCRIPT)], capture_output=True, encoding="utf-8")
        self.assertEqual(done.returncode, 2)
        self.assertIn("keygen", done.stdout)


if __name__ == "__main__":
    unittest.main()
