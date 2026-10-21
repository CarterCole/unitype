#!/usr/bin/env python3
"""Serves the test pages on 127.0.0.1, so the proofs have something to fetch.

Each page is sent with the Content-Type that pages/types.json gives it. That is
the part a proof can get wrong: the bytes of latin1.txt only read as "café"
if the declared charset is honoured.

    python3 serve.py          # port 8765, which the vectors in conform/ use
    python3 serve.py 9000     # somewhere else
"""

import json
import pathlib
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PAGES = pathlib.Path(__file__).resolve().parent / "pages"
TYPES = json.loads((PAGES / "types.json").read_text())


class Pages(BaseHTTPRequestHandler):
    def do_GET(self):
        name = self.path.lstrip("/")
        if name not in TYPES:
            self.send_error(404, f"no page called {name!r}")
            return
        body = (PAGES / name).read_bytes()
        self.send_response(200)
        self.send_header("Content-Type", TYPES[name])
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8765
    print(f"serving {PAGES} on http://127.0.0.1:{port}/", flush=True)
    ThreadingHTTPServer(("127.0.0.1", port), Pages).serve_forever()
