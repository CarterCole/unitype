#!/usr/bin/env python3
"""`net::parse_v4`, `format_v4`, `parse_v6` and `format_v6`, proved with the
standard library's `ipaddress`.

The harness this imports is generated: run `unitype gen` in the project first,
and it appears under generated/python. What is written by hand is the four
functions, and the lines saying which entry each answers to.
"""

import ipaddress
import os
import sys

sys.path.insert(
    0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "generated", "python")
)

from unitype_harness import unitype_main  # noqa: E402 — found by the line above


def parse_v4(text):
    """`IPv4Address(text)` is the parser: a strict dotted quad, with leading
    zeros refused since 3.9.5, which is what the proposition asks for."""
    try:
        return list(ipaddress.IPv4Address(text).packed)
    except ValueError:
        return None


def format_v4(ip):
    return str(ipaddress.IPv4Address(bytes(ip)))


def parse_v6(text):
    """The proposition admits an address and nothing else, so a zone, which
    `IPv6Address` has accepted since 3.9, is refused here first."""
    if "%" in text:
        return None
    try:
        packed = ipaddress.IPv6Address(text).packed
    except ValueError:
        return None
    return [int.from_bytes(packed[i : i + 2], "big") for i in range(0, 16, 2)]


def format_v6(ip):
    """`str()` compresses as RFC 5952 says, except that an IPv4-mapped address
    comes out in hexadecimal; the RFC recommends the dotted quad, and so does
    the proposition, so that case is written here."""
    addr = ipaddress.IPv6Address(b"".join(h.to_bytes(2, "big") for h in ip))
    mapped = addr.ipv4_mapped
    if mapped is not None:
        return f"::ffff:{mapped}"
    return str(addr)


if __name__ == "__main__":
    unitype_main(
        {
            "ipaddress.IPv4Address": parse_v4,
            "str(ipaddress.IPv4Address)": format_v4,
            "ipaddress.IPv6Address": parse_v6,
            "str(ipaddress.IPv6Address)": format_v6,
        }
    )
