// `fetch::fetch_text`, proved with the `fetch` that every Node since 18 has
// built in.
//
// The harness this imports is generated: run `unitype gen` in the project
// first, and it appears under generated/typescript. Node 24 or later runs this
// file as it is, by dropping the types.

import { unitypeMain } from "../generated/typescript/unitype_harness.ts";

async function fetchText(url: string): Promise<string> {
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(`${response.status} ${response.statusText}`);
  }
  // `response.text()` would decode as UTF-8 whatever the server declared, which
  // is the one thing the proposition says not to do. So the charset is read
  // here, and decoding is fatal on a byte that does not belong.
  const declared = /;\s*charset\s*=\s*"?([^";\s]+)/i.exec(
    response.headers.get("content-type") ?? "",
  );
  const decoder = new TextDecoder(declared?.[1] ?? "utf-8", { fatal: true });
  return decoder.decode(await response.arrayBuffer());
}

unitypeMain({ fetch: fetchText });
