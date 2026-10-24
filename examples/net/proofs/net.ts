// `net::parse_v4`, `format_v4`, `parse_v6` and `format_v6` in Node.
//
// Node has `net.isIP`, which says whether text is an address and never which
// address it is, so the parsers here are written out. That is the point of
// having the proposition: what holds this file to the same answers as
// `ipaddress` and `std::net` is `unitype conform`, not anyone's care.
//
// The harness this imports is generated: run `unitype gen` in the project
// first, and it appears under generated/typescript. Node 24 or later runs this
// file as it is, by dropping the types.

import { unitypeMain } from "../generated/typescript/unitype_harness.ts";

const DOTTED_QUAD = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/;
const HEXTET = /^[0-9a-fA-F]{1,4}$/;

function parseIPv4(text: string): number[] | null {
  const m = DOTTED_QUAD.exec(text);
  if (!m) return null;
  // A leading zero is refused rather than read, since platforms have read it
  // as octal and as decimal both.
  const octets = m.slice(1).map((s) => (s.length > 1 && s.startsWith("0") ? -1 : Number(s)));
  return octets.every((o) => o >= 0 && o <= 255) ? octets : null;
}

function formatIPv4(ip: number[]): string {
  return ip.join(".");
}

/// The groups one side of a `::` spells; a dotted quad is allowed only where
/// `last` says these are the final groups of the whole address.
function hextets(part: string, last: boolean): number[] | null {
  if (part === "") return [];
  const out: number[] = [];
  const pieces = part.split(":");
  for (let i = 0; i < pieces.length; i++) {
    const piece = pieces[i];
    if (last && i === pieces.length - 1 && piece.includes(".")) {
      const v4 = parseIPv4(piece);
      if (!v4) return null;
      out.push(v4[0] * 256 + v4[1], v4[2] * 256 + v4[3]);
    } else if (HEXTET.test(piece)) {
      out.push(parseInt(piece, 16));
    } else {
      return null;
    }
  }
  return out;
}

function parseIPv6(text: string): number[] | null {
  const halves = text.split("::");
  if (halves.length > 2) return null;
  if (halves.length === 1) {
    const all = hextets(halves[0], true);
    return all && all.length === 8 ? all : null;
  }
  const head = hextets(halves[0], false);
  const tail = hextets(halves[1], true);
  if (!head || !tail) return null;
  // `::` stands for at least one group of zeros.
  const zeros = 8 - head.length - tail.length;
  if (zeros < 1) return null;
  return [...head, ...Array<number>(zeros).fill(0), ...tail];
}

function formatIPv6(ip: number[]): string {
  // An IPv4-mapped address: RFC 5952 recommends the dotted quad for its last
  // four bytes.
  if (ip.slice(0, 5).every((h) => h === 0) && ip[5] === 0xffff) {
    return `::ffff:${formatIPv4([ip[6] >> 8, ip[6] & 255, ip[7] >> 8, ip[7] & 255])}`;
  }
  // The longest run of two or more zero groups becomes `::`, the first when
  // two are as long.
  let best = { at: -1, len: 0 };
  for (let i = 0; i < 8; ) {
    if (ip[i] !== 0) {
      i++;
      continue;
    }
    let j = i;
    while (j < 8 && ip[j] === 0) j++;
    if (j - i >= 2 && j - i > best.len) best = { at: i, len: j - i };
    i = j;
  }
  const hex = (groups: number[]) => groups.map((h) => h.toString(16)).join(":");
  if (best.at < 0) return hex(ip);
  return `${hex(ip.slice(0, best.at))}::${hex(ip.slice(best.at + best.len))}`;
}

unitypeMain({ parseIPv4, formatIPv4, parseIPv6, formatIPv6 });
