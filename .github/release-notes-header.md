## Installation

PyxeeTB runs on **Linux x86_64** (built for a small server such as an Oracle E2.Micro) as one static binary with nothing else to install.

Download **`pyxeetb-x.y.z-x86_64-unknown-linux-musl.tar.gz`** below, check it, and unpack it:

```sh
sha256sum -c SHA256SUMS.txt --ignore-missing
tar -xzf pyxeetb-*-x86_64-unknown-linux-musl.tar.gz
./pyxeetb-*/pyxeetb --version
```

If the release carries `SHA256SUMS.txt.sig`, that file is an Ed25519 signature of the checksum list (`python scripts/sign_sums.py verify SHA256SUMS.txt <public key>`), so the list itself can be trusted.
