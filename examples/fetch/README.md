# fetch

One proposition, `fetch::fetch_text(url: str) -> future<str>`, and three proofs
of it in three languages, each started as its own process and spoken to over
standard input and output:

| Proof | Language | Calls it |
|---|---|---|
| `proofs/fetch_text.py` | Python | `urllib.request.urlopen` |
| `proofs/fetch_text.ts` | TypeScript, run by Node 24 or later | `fetch` |
| `rust/` | Rust | `ureq::get` |

The proposition's doc says what "text" means — decoded by the declared charset,
UTF-8 when there is none, strict about bytes that do not belong — because each
platform's shortest way of fetching text decides those things differently.
Saying it once is what lets the three be held to one answer.

## Running it

From the repository root:

```bash
# The Python and TypeScript proofs import a harness that unitype generates.
cargo run -p unitype-cli -- --project examples/fetch gen

# The Rust proof is a binary. Until it is built, its environment is simply not
# there, and conform says so rather than counting it as a disagreement.
cargo build --release --manifest-path examples/fetch/rust/Cargo.toml

# The pages the vectors fetch, each sent with the Content-Type in pages/types.json.
python3 examples/fetch/serve.py &

# One proof, chosen by policy, or one in particular.
cargo run -p unitype-cli -- --project examples/fetch run fetch::fetch_text \
    --args '["http://127.0.0.1:8765/latin1.txt"]' --impl fetch_text.ureq -v

# All three, held to each other and to the expected answers in conform/.
cargo run -p unitype-cli -- --project examples/fetch conform
```

## What is checked

`conform/fetch_text.json` fetches plain ASCII, declared UTF-8, declared Latin-1,
undeclared UTF-8, an empty page, and a longer page with no expected answer, so
there the proofs are held only to each other.

The Latin-1 page is the one that tells proofs apart. Its bytes read as `café`
only if the declared charset is honoured; Node's `response.text()` decodes as
UTF-8 regardless and returns `caf�`. The CLI tests add a proof written that way
and check that `conform` catches it.

Charset labels are read as the WHATWG Encoding Standard reads them in Node and
Rust (`iso-8859-1` means windows-1252) and as Python's codecs read them
(`iso-8859-1` means Latin-1). The two differ only for bytes 0x80 to 0x9F, which
no page here contains; a page that did would be a disagreement worth finding.
