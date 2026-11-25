#!/usr/bin/env python3
"""`monad_test::explain`, proved in Python.

Each instantiation is a constructor with its own `pure` and `bind`, and the
samples the laws are tried on. A verdict says whether the constructor passed,
and if not, the first part that failed: a missing member, or the first law that
breaks on the samples. conform holds this to the same verdicts as the other
proofs.

The harness this imports is generated: run `unitype gen` in the project first,
and it appears under generated/python.
"""

import os
import sys

sys.path.insert(
    0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "generated", "python")
)

from unitype_harness import unitype_main  # noqa: E402 — found by the line above


def _list_bind(m, step):
    return [y for x in m for y in step(x)]


def _optional_bind(m, step):
    return None if m is None else step(m)


def _first_list_bind(m, step):
    """Keeps only the first answer each step gives back: not a monad, since it
    loses what the step said."""
    out = []
    for x in m:
        answer = step(x)
        if answer:
            out.append(answer[0])
    return out


INSTANCES = {
    "list": {
        "pure": lambda a: [a],
        "bind": _list_bind,
        "values": [3, 0, 7],
        "monadic": [[1, 2], [], [4]],
        "f": lambda x: [x, x * 2],
        "g": lambda y: [y + 1],
    },
    "optional": {
        "pure": lambda a: a,
        "bind": _optional_bind,
        "values": [3, 0, 7],
        "monadic": [5, None],
        "f": lambda x: x + 1 if x < 10 else None,
        "g": lambda y: y * 2 if y < 100 else None,
    },
    "first_list": {
        "pure": lambda a: [a],
        "bind": _first_list_bind,
        "values": [3, 0, 7],
        "monadic": [[1, 2], [], [4]],
        "f": lambda x: [x, x * 2],
        "g": lambda y: [y + 1],
    },
}


def _first_law_failed(inst):
    """The first law that breaks on the samples, or None if none does."""
    pure, bind, f, g = inst["pure"], inst["bind"], inst["f"], inst["g"]
    for a in inst["values"]:
        # Left identity: lifting a value and then stepping is the step itself.
        if bind(pure(a), f) != f(a):
            return "left_identity"
    for m in inst["monadic"]:
        # Right identity: stepping into `pure` changes nothing.
        if bind(m, pure) != m:
            return "right_identity"
        # Associativity: grouping the two steps either way gives one answer.
        if bind(bind(m, f), g) != bind(m, lambda x: bind(f(x), g)):
            return "associativity"
    return None


def _verdict(held, reason=None):
    return {"held": held, "reason": reason}


def explain(name):
    inst = INSTANCES.get(name)
    if inst is None:
        raise ValueError(f"no instantiation of this test for {name!r}")
    if not callable(inst.get("pure")):
        return _verdict(False, "no_pure")
    if not callable(inst.get("bind")):
        return _verdict(False, "no_bind")
    law = _first_law_failed(inst)
    return _verdict(law is None, law)


if __name__ == "__main__":
    unitype_main({"monad_test.explain": explain})
