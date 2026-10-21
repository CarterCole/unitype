#!/usr/bin/env python3
"""`fetch::fetch_text`, proved with nothing but the standard library.

The harness this imports is generated: run `unitype gen` in the project first,
and it appears under generated/python. What is written by hand is the function,
and the line saying which entry it answers to.
"""

import os
import sys
import urllib.request

sys.path.insert(
    0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "generated", "python")
)

from unitype_harness import unitype_main  # noqa: E402 — found by the line above


def fetch_text(url):
    """Fetches a page and decodes it the way the server said it is encoded.

    `urlopen` raises for a status of 400 or more, and the harness reports that
    as this proof failing, which is what the proposition asks for. Decoding is
    strict, so a byte that is not valid in the declared charset fails too.
    """
    with urllib.request.urlopen(url, timeout=10) as response:
        charset = response.headers.get_content_charset() or "utf-8"
        return response.read().decode(charset)


if __name__ == "__main__":
    unitype_main({"urllib.request.urlopen": fetch_text})
