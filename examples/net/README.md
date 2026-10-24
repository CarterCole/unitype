# net

Addresses as the data they are, and the text as two propositions about them.

`net::IpV4` is `[octet; 4]`, an `octet` being `i32[0..255]`: a list of exactly
four integers, each between 0 and 255. The checker refuses `[256, 1, 1, 1]`
with no parser involved. Generated Rust spells it `[u8; 4]`, which is what
`Ipv4Addr::from` takes; `IpV6` is `[hextet; 8]` and comes out `[u16; 8]`.
What they look like as text is a proposition with three proofs each:

| Proposition | Python | Node | Rust |
|---|---|---|---|
| `parse_v4(text: str) -> IpV4?` | `ipaddress.IPv4Address` | written out | `Ipv4Addr::from_str` |
| `format_v4(ip: IpV4) -> str` | `str()` | `join(".")` | `Ipv4Addr::to_string` |
| `parse_v6(text: str) -> IpV6?` | `ipaddress.IPv6Address` | written out | `Ipv6Addr::from_str` |
| `format_v6(ip: IpV6) -> str` | `str()` | written out | `Ipv6Addr::to_string` |

Node has `net.isIP`, which says whether text is an address and never which
address it is, so its parsers are written by hand. The proposition is what holds
them to the same answers as `ipaddress` and `std::net`.

The propositions say the things the platforms decide differently. A leading
zero in a dotted quad is refused, because it has been read as octal and as
decimal both. A zone (`fe80::1%eth0`) is not part of an address, though
Python's parser has accepted one since 3.9. An IPv4-mapped address is written
with a dotted quad, as RFC 5952 recommends and as Rust does, though Python
writes it in hexadecimal. Each of those is one line in a proof, and `conform`
is what shows the line is there.

## Running it

From the repository root:

```bash
# The Python and TypeScript proofs import a harness that unitype generates.
cargo run -p unitype-cli -- --project examples/net gen

# The Rust proof is a binary. Until it is built, its environment is simply not
# there, and conform says so rather than counting it as a disagreement.
cargo build --release --manifest-path examples/net/rust/Cargo.toml

# The types, with their bounds, and the words every platform's type shares.
cargo run -p unitype-cli -- --project examples/net ls types

# One proof, chosen by policy, or one in particular.
cargo run -p unitype-cli -- --project examples/net run net::parse_v6 \
    --args '["2001:db8::1"]' --impl parse_v6.std -v

# All three, held to each other and to the expected answers in conform/.
cargo run -p unitype-cli -- --project examples/net conform
```

## What is checked

Sixty-six cases across the four propositions: the forms every reader must
accept, and the near-misses each must refuse — an octet over 255, a leading
zero, a zone, two `::`, a `::` standing for no groups, a dotted quad anywhere
but the end. Every parse case gives its answer, `null` where the text is not an
address; every format case gives the one spelling RFC 5952 allows.
