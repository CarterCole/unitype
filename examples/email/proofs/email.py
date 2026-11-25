#!/usr/bin/env python3
"""`email::contacted` and `email::verified`, proved over simulated mailboxes.

A receiver names the mailbox that answers for its address. An inbox delivers to
any address and answers for the ones it owns; a bounce delivers to none. Nothing
here sends mail: the mailboxes are the fixture the proofs are tested against.

The harness this imports is generated: run `unitype gen` in the project first,
and it appears under generated/python.
"""

import os
import sys

sys.path.insert(
    0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "generated", "python")
)

from unitype_harness import unitype_main  # noqa: E402 — found by the line above

MAILBOXES = {
    "inbox": {"answers_for": {"ada@example.com"}},
    "bounce": {"answers_for": set()},
}


def contacted(receiver):
    """Mail sent to the address is delivered: an inbox takes any address with
    an at sign in it, and a bounce takes none."""
    return receiver["provider"] == "inbox" and "@" in receiver["address"]


def verified(receiver):
    """The mailbox answers for the address only when it owns it."""
    return receiver["address"] in MAILBOXES[receiver["provider"]]["answers_for"]


if __name__ == "__main__":
    unitype_main({"email.contacted": contacted, "email.verified": verified})
