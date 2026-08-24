#!/usr/bin/env python3
"""A Python environment offering one proof, over standard input and output.

This is the whole of what it takes for another language to take part. There is
no library to install and nothing to link against: read one JSON message, do the
work, write one JSON message back.

The harness at the bottom is the same for every Python module that wants to
participate, and code generation emits it. What a person writes is the function
above it, and the line that says which propositions it proves.
"""

import json
import sys


def bubble_sort(xs):
    """Sorts by repeatedly swapping neighbours that are in the wrong order.

    Quadratic in time and constant in extra space, which is exactly why it is
    here: it is the wrong choice when speed matters and the right one when
    memory does. The dispatcher decides which of those it is at the moment of
    the call.
    """
    xs = list(xs)
    for i in range(len(xs)):
        for j in range(len(xs) - i - 1):
            if xs[j] > xs[j + 1]:
                xs[j], xs[j + 1] = xs[j + 1], xs[j]
    return xs


# What this module can prove, by the entry name a manifest points at.
PROVES = {
    "bubble_sort": bubble_sort,
}


def main():
    # One message per line. Reading lines serves both ways of being called: if
    # the parent sends one message and closes the stream, this loop runs once
    # and the process exits; if the parent keeps the stream open, it keeps
    # answering. Nothing here has to know which is happening.
    for line in sys.stdin:
        line = line.strip()
        if line:
            answer(line)


def answer(line):
    try:
        request = json.loads(line)
    except ValueError as exc:
        return fail("bad_request", f"could not read the request: {exc}")

    if request.get("unitype") != "1":
        return fail(
            "bad_version",
            f"this module speaks version 1, not {request.get('unitype')!r}",
        )
    if request.get("kind") != "invoke":
        return fail("bad_kind", f"cannot do {request.get('kind')!r}")

    entry = request.get("entry")
    proof = PROVES.get(entry)
    if proof is None:
        known = ", ".join(sorted(PROVES)) or "nothing"
        return fail("no_such_entry", f"this module proves {known}, not {entry!r}")

    try:
        return succeed(proof(*request.get("args", [])))
    except Exception as exc:  # noqa: BLE001 — anything it raises is its answer
        return fail(type(exc).__name__, str(exc))


def succeed(value):
    _send({"ok": True, "value": value})


def fail(code, message):
    # Reported as a failure of the proof, not of the process: exiting non-zero
    # would say the environment is broken, which it is not.
    _send({"ok": False, "error": {"code": code, "message": message}})


def _send(response):
    # One line, flushed: a parent waiting on the next line would otherwise wait
    # on a buffer that never fills.
    json.dump(response, sys.stdout)
    sys.stdout.write("\n")
    sys.stdout.flush()


if __name__ == "__main__":
    main()
