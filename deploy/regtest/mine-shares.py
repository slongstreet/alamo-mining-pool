#!/usr/bin/env python3
"""Dev-only stratum CPU miner for the regtest pool. Standard library only.

Connects to one lane (chosen by port), subscribes, authorizes, and submits shares for a
while, so the dashboard has workers, shares, and hashrate to show. Regtest difficulty is
trivial, so most shares are blocks too: it also fills the block history (and mines real
regtest blocks on that lane's chain).

  deploy/regtest/mine-shares.py --port 3333 --seconds 60          # LTC + DOGE lane, scrypt
  deploy/regtest/mine-shares.py --port 3334 --worker rig2         # BTC lane, SHA-256d

Never point this at anything but a regtest pool: it is slow and pays regtest addresses.
"""

import argparse
import hashlib
import json
import socket
import struct
import time

# Regtest addresses (the ones config/alamo.regtest.toml uses as fallbacks).
ADDRESSES = {
    "scrypt": ("rltc1qyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zwc5c93", "midSACfDe3qAxJZZXA9gkwBZgPqJJUpy1w"),
    "sha256d": ("bcrt1qyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zs4w3j0", "x"),
}
SHARE_MULTIPLIER = {"scrypt": 65536.0, "sha256d": 1.0}
DIFF1 = 0xFFFF << 208


def sha256d(data: bytes) -> bytes:
    return hashlib.sha256(hashlib.sha256(data).digest()).digest()


def pow_hash(algo: str, header: bytes) -> bytes:
    if algo == "scrypt":
        return hashlib.scrypt(header, salt=header, n=1024, r=1, p=1, dklen=32)
    return sha256d(header)


def share_target(algo: str, difficulty: float) -> int:
    network = difficulty / SHARE_MULTIPLIER[algo]
    return (DIFF1 << 32) // max(1, int(network * 2**32))


class Stratum:
    def __init__(self, host: str, port: int):
        self.sock = socket.create_connection((host, port), timeout=30)
        self.buffer = b""
        self.next_id = 1
        self.difficulty = 1.0
        self.job = None

    def send(self, method: str, params: list) -> int:
        req_id = self.next_id
        self.next_id += 1
        self.sock.sendall((json.dumps({"id": req_id, "method": method, "params": params}) + "\n").encode())
        return req_id

    def read(self) -> dict:
        while b"\n" not in self.buffer:
            chunk = self.sock.recv(65536)  # raises socket.timeout, keeping the buffer
            if not chunk:
                raise ConnectionError("pool closed the connection")
            self.buffer += chunk
        line, self.buffer = self.buffer.split(b"\n", 1)
        msg = json.loads(line)
        if msg.get("method") == "mining.set_difficulty":
            self.difficulty = float(msg["params"][0])
        elif msg.get("method") == "mining.notify":
            self.job = msg["params"]
        return msg

    def call(self, method: str, params: list) -> dict:
        req_id = self.send(method, params)
        while True:
            msg = self.read()
            if msg.get("id") == req_id:
                return msg

    def drain(self, wait: float = 0.0):
        """Read notifications already waiting (or arriving within `wait` seconds)."""
        self.sock.settimeout(wait or 0.001)
        try:
            while True:
                self.read()
        except (socket.timeout, TimeoutError):
            pass
        finally:
            self.sock.settimeout(30)


def header_for(job: list, extranonce1: bytes, extranonce2: bytes, nonce: int) -> bytes:
    _job_id, prevhash, coinb1, coinb2, branch, version, nbits, ntime = job[:8]
    coinbase = bytes.fromhex(coinb1) + extranonce1 + extranonce2 + bytes.fromhex(coinb2)
    root = sha256d(coinbase)
    for step in branch:
        root = sha256d(root + bytes.fromhex(step))
    # Stratum sends the previous hash as eight byte-swapped 32-bit words.
    prev = b"".join(bytes.fromhex(prevhash)[i : i + 4][::-1] for i in range(0, 32, 4))
    return (
        struct.pack("<I", int(version, 16))
        + prev
        + root
        + struct.pack("<I", int(ntime, 16))
        + struct.pack("<I", int(nbits, 16))
        + struct.pack("<I", nonce)
    )


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=3333, help="the lane's stratum port (3333 LTC+DOGE, 3334 BTC)")
    ap.add_argument("--algo", choices=["scrypt", "sha256d"], help="default: sha256d on 3334, else scrypt")
    ap.add_argument("--seconds", type=float, default=60, help="how long to mine")
    ap.add_argument("--interval", type=float, default=2, help="minimum seconds between shares")
    ap.add_argument("--worker", default="cpu", help="worker name after the address")
    ap.add_argument("--address", help="payout address (default: a regtest address for the lane)")
    args = ap.parse_args()
    algo = args.algo or ("sha256d" if args.port == 3334 else "scrypt")
    address, password = ADDRESSES[algo]
    address = args.address or address

    pool = Stratum(args.host, args.port)
    sub = pool.call("mining.subscribe", ["alamo-mine-shares/0.1"])
    extranonce1 = bytes.fromhex(sub["result"][1])
    extranonce2_size = int(sub["result"][2])
    auth = pool.call("mining.authorize", [f"{address}.{args.worker}", password])
    if not auth.get("result"):
        raise SystemExit(f"authorize refused: {auth.get('error')}")
    pool.drain(wait=2)
    print(f"{algo} on port {args.port} as {address}.{args.worker}")

    accepted = rejected = 0
    counter = 0
    deadline = time.time() + args.seconds
    while time.time() < deadline:
        pool.drain()
        if pool.job is None:
            pool.drain(wait=1)
            continue
        job = pool.job
        target = share_target(algo, pool.difficulty)
        counter += 1
        extranonce2 = counter.to_bytes(extranonce2_size, "big")
        for nonce in range(0, 1 << 32):
            h = pow_hash(algo, header_for(job, extranonce1, extranonce2, nonce))
            if int.from_bytes(h, "little") <= target:
                break
            if nonce % 2048 == 2047 and time.time() >= deadline:
                break
        else:
            continue
        if time.time() >= deadline:
            break
        reply = pool.call(
            "mining.submit",
            [f"{address}.{args.worker}", job[0], extranonce2.hex(), job[7], f"{nonce:08x}"],
        )
        if reply.get("result"):
            accepted += 1
        else:
            rejected += 1
            print(f"rejected: {reply.get('error')}")
        time.sleep(args.interval)
    print(f"done: {accepted} accepted, {rejected} rejected")


if __name__ == "__main__":
    main()
