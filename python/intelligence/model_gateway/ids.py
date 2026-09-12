"""Canonical identifier generation for gateway-owned objects (DOMAIN.md §1.1).

`ModelRoute` is an intelligence-plane object, so its `mr_<ULID>` identity is minted here
rather than by the Rust authority that owns runtime entities. The ULID is time-sortable and
carries a digest-derived randomness field so the same (call, model, rule) triple yields the
same id, which keeps route identity reproducible in tests without a clock dependency.
"""

from __future__ import annotations

import hashlib
import os
import time

CROCKFORD_ALPHABET = "0123456789ABCDEFGHJKMNPQRSTVWXYZ"
_ULID_LENGTH = 26
_SEED_BYTES = 10  # 80 bits of ULID randomness


def new_ulid(*, seed: str | None = None, timestamp_ms: int | None = None) -> str:
    """A 26-character Crockford base32 ULID; `seed` makes the random field reproducible."""
    moment = int(time.time() * 1000) if timestamp_ms is None else timestamp_ms
    if seed is None:
        randomness = os.urandom(_SEED_BYTES)
    else:
        randomness = hashlib.blake2b(seed.encode("utf-8"), digest_size=_SEED_BYTES).digest()
    value = (moment << 80) | int.from_bytes(randomness, "big")
    encoded = []
    for _ in range(_ULID_LENGTH):
        encoded.append(CROCKFORD_ALPHABET[value & 0x1F])
        value >>= 5
    return "".join(reversed(encoded))
