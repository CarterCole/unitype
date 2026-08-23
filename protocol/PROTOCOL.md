# The unitype protocol

**Version 1.0 (schema), hash domains `v1`, wordlist `bip39-english-v1`.**

This is the normative specification. An implementation in any language that
follows it will compute the same canonical forms, the same content addresses,
and the same word aliases as any other. The Rust workspace in this repository is
the reference implementation, not the definition.

Sections marked **frozen** must not change without a version bump, because
changing them changes every address already published.

Status: §§1–5 and §7 are settled and implemented. §§6, 8, and 9 are specified as
the layers above them are built; they are marked accordingly.

---

## 1. Model

A **type** describes a set of values. A **value** inhabits a type. A
**universe** is a named scope of type declarations and function signatures. A
**workspace** is a set of universes together with the **links** between them.

A **function signature** is a proposition. An **implementation** is a proof of
it. A signature may have many implementations, in different languages, with
different costs; all must agree on results, and the system checks this rather
than assuming it.

Two rules govern compatibility, and they differ deliberately:

- **Within a universe, typing is nominal.** Two structs with identical fields
  are different types unless one declares that it extends the other.
- **Across universes, nothing is compatible.** Subtyping does not cross a
  universe boundary at all. Data crosses only through a declared link (§8),
  which always names how the conversion is witnessed.

The second rule is the point, not a limitation. A system that silently accepted
one universe's `User` as another's would have no way to say the two are
different, which is the situation this protocol exists to fix.

### 1.1 Types

```
Type ::= never | any | unit | bool | i32 | i64 | f64 | string | bytes
       | literal(LiteralValue)
       | optional(Type) | list(Type) | map(Type, Type) | tuple(Type*)
       | record(Field*) | union(Type*) | function(Type*, Type)
       | future(Type)
       | ref(universe, name, Type*)
       | param(name)

LiteralValue ::= bool(b) | int(i) | str(s)
Field        ::= (name, Type, doc?)
```

Notes on the choices, each of which affects what is representable:

- `never` is the bottom type and `any` the top. An empty union is `never`.
- Integers are `i32` and `i64` only; floats are `f64` only. Unsigned and
  arbitrary-precision integers are excluded because they do not survive a trip
  through a JavaScript `number`.
- **Numeric widening is allowed only where nothing is lost.** `i32` may be used
  where `i64` or `f64` is expected, because every 32-bit integer is exactly both.
  `i64` may **not** be used where `f64` is expected: past 2^53 an integer does
  not survive the conversion, and an implementation must not lose that silently.
  A *specific* integer — a literal, or a value being validated — is accepted as
  an `f64` when its magnitude is at most 2^53. Note that a round-trip cast is not
  a valid test for this, because converting an out-of-range float back to an
  integer saturates.
- `literal` has no float case. Floats have no exact equality worth building type
  identity on.
- **Absence has one representation.** `optional(T)` covers "explicitly null",
  "field not present", and "none". This collapses a distinction some languages
  make, and does so knowingly: it is the only account of absence that all target
  languages share.
- `union` is untagged and structural. A tagged sum is a declared `enum`, which
  is nominal and has a fixed wire tag.
- `function` is structural and positional. Parameter *names* belong to the
  declaration, not the type, so two signatures differing only in parameter
  naming state the same proposition.
- `future(T)` is a first-class constructor, not a calling convention.
  `future(future(T))` is **not** collapsed: it is a distinct type in languages
  that distinguish it, and flattening is a backend's business (§7).

A `map` key type must be `string`, `i32`, `i64`, a non-boolean literal, or a
union of those, so that a map is representable as an object in every target.

### 1.2 Identifiers **(frozen)**

An identifier matches `[A-Za-z_][A-Za-z0-9_]*`, ASCII only.

ASCII is required because identifiers feed the canonical byte encoding: under
Unicode, two identifiers that look identical could normalize differently and
hash differently. Relaxing this later is compatible, since every identifier
valid now stays valid and encodes identically.

Because an identifier cannot begin with `$`, keys beginning with `$` are
reserved for the value encoding (§4) and can never collide with a field name.

A **qualified name** is written `universe::name`.

---

## 2. Serialization **(frozen)**

Every type and declaration is a JSON object with a `"k"` discriminator holding
the kind token. Absent optional members and empty collections are **omitted**,
never written as `null` or `[]`.

```json
{"k": "optional", "of": {"k": "list", "of": {"k": "i64"}}}

{"k": "struct", "name": "Circle",
 "extends": {"universe": "demo", "name": "Shape"},
 "fields": [{"name": "radius", "ty": {"k": "f64"}}]}
```

Kind tokens for types: `never any unit bool i32 i64 f64 string bytes literal
optional list map tuple record union function future ref param`.
For declarations: `struct interface enum alias`.
For expressions (§6): `lit var field make_record make_list make_tuple
make_variant call let if map_through`.

A workspace document:

```json
{"version": "1.0",
 "universes": [{"name": "demo", "types": [...], "functions": [...]}],
 "links": [...]}
```

An implementation MUST refuse a `version` it does not understand rather than
reading what it recognizes.

### 2.1 Input shorthand (non-normative)

On input a type may also be written as a string. Shorthand is a convenience for
authors and is expanded before anything else happens; it never appears in
canonical output.

| Shorthand | Expands to |
|---|---|
| `"i64"`, `"bool"`, … | that primitive |
| `"int"`, `"float"`, `"str"`, `"boolean"`, `"null"` | `i64`, `f64`, `string`, `bool`, `unit` |
| `"User"` | a reference in the enclosing universe |
| `"geo::Point"` | a reference to another universe |
| `"T?"` | `optional(T)` |
| `"[T]"` | `list(T)` |

Expansion is directed by the schema, so a string is expanded only where a type
belongs. Under `{"k":"map"}` the `value` member is a type and is expanded; under
`{"k":"lit"}` it is a value and is left alone.

---

## 3. Canonical form **(frozen)**

Normalization reduces every type to the single form that represents it. The
rules are purely syntactic: no rule consults a workspace or decides a subtyping
question, so a type can be normalized in isolation and the result never depends
on what else is loaded. Semantic redundancy — `i32 | i64` collapsing — is a
matter for the checker, deliberately left out of canonical form.

Applied bottom-up to a fixed point:

**Unions**
1. Nested unions flatten.
2. `never` members are dropped. A member of `any` makes the union `any`.
3. A member of `optional(X)` is replaced by `X`, and the whole union becomes
   optional. A member of `unit` is dropped and the union becomes optional.
   *After normalization, no union directly contains `unit` or `optional`.*
4. `literal(true) | literal(false)` becomes `bool`.
5. A `literal(v)` is dropped if the union contains its base type — `bool`,
   `i64`, or `string` — or contains `i32` and `v` is an integer in `i32` range.
6. Members are deduplicated and **sorted ascending by their own canonical
   bytes**. This is a total order requiring no separate ranking table.
7. An empty union is `never`; a one-member union is that member.

**Optionals**
8. `optional(optional(T))` → `optional(T)`; `optional(never)` and
   `optional(unit)` → `unit`; `optional(any)` → `any`.

**Tuples**
9. A tuple of none is `unit`; a tuple of one is its element.

**Records and structs**
10. Anonymous `record` fields sort by name. **Struct fields do not sort**: their
    declared order is semantic and drives generated code.

**References**
11. Canonical form always names the universe. An unqualified reference means the
    universe it was written in, which is context the type itself does not carry;
    once qualified, a type means the same thing wherever it is read.

**Registry order**
12. Universes, declarations, signatures, and links sort by name. Registry order
    is not semantic.

Normalization is idempotent, and an implementation SHOULD test that it is.

---

## 4. Values **(frozen)**

A value is JSON, extended with four cases JSON has no shape for. Each is a
single-key object whose key begins with `$`:

| Value | JSON |
|---|---|
| bytes | `{"$bytes": "<base64 standard, padded>"}` |
| tuple | `{"$tuple": [...]}` |
| variant | `{"$tag": "Name", "value": ...}` |
| map | `{"$map": [[k, v], ...]}` |

`value` is omitted from a variant carrying nothing. A plain JSON object decodes
to a **record**; a map with string keys is recognized only when a value is read
against a known type.

Integers and floats are distinct: JSON `1` is an integer and `1.0` is a float,
and the distinction survives a round trip. `NaN` and the infinities are
**rejected** at the encoding boundary rather than approximated, since JSON
cannot carry them.

An unrecognized key alongside a `$` escape is an error, so that a typo is
reported instead of silently dropped.

---

## 5. Content addressing **(frozen)**

A type's identity is a hash of the canonical bytes of its **identity form** —
the declaration with everything that is not the type itself removed.

Identity **excludes**:
- documentation and metadata, because prose is not structure;
- parameter names in signatures, because `sort(xs)` and `sort(items)` state the
  same proposition;
- a signature's body, because a proposition does not change when someone finds a
  better proof of it;
- a signature's `refines` relationship, which is a claim about two propositions
  rather than part of either.

Generic parameters are renamed to their positions (`_0`, `_1`, …) before
hashing, so parameter naming is not identity either.

### 5.1 Hash construction

`blake3` in keyed-derivation mode. The domain string separates the kinds so that
two different things can never collide even if their encodings coincide:

| Hash | Domain string | Over |
|---|---|---|
| local | `unitype local v1` | a type expression, references left as written |
| declaration | `unitype decl v1` | a declaration as authored, names kept |
| signature | `unitype sig v1` | a proposition |
| alias | `unitype alias v1` | an address, to derive words (§5.3) |

A **structural** hash, which erases names by resolving references across a whole
workspace, is built on these primitives once resolution exists; it is specified
in §5.4 and not yet frozen.

### 5.2 Text form **(frozen)**

`ut1:` followed by 64 lowercase hex digits. The first twelve digits may be shown
where a full address would crowd everything else out; they are an abbreviation,
never an identity.

### 5.3 Word aliases **(frozen)**

For speech and sharing, not ground truth. A hash of an unbounded space into
fixed word tuples cannot be injective, and aliases are never authoritative.

The convention follows `interval_clock`'s §7, applied to types:

- Wordlist: the frozen **BIP-39 English list**, 2048 words, 11 bits per word,
  first four letters unique. Versioned `bip39-english-v1`.
- Derivation: `blake3_derive_key("unitype alias v1", address_bytes)` → the top
  **121 bits** → **11 words**, hyphen-joined, read most-significant bit first.
- The shareable alias is the **shortest prefix unique among the addresses in
  hand, never fewer than 3 words** (~33 bits). Prefixes behave like abbreviated
  git hashes: showing more words is always correct, and a collision lengthens
  the newcomer's alias rather than changing anyone else's.

Because words derive from the address and the address derives from the
structure, two parties who have never communicated derive the same words for the
same type.

Shortening is relative to a set. A persistent, federatable alias registry — with
`full_alias, address, prefix_len, wordlist` rows, so that minted aliases never
change as a corpus grows — is a later addition and is not required for
interoperability.

### 5.4 Structural hashing *(specified, not yet frozen)*

Erasing names requires resolving references, so this is defined over a checked
workspace. Aliases are expanded first, making an alias exactly a name: its
structural hash equals its target's. The declaration graph is decomposed into
strongly connected components; each component is encoded with its declaration
names erased, generic parameters renamed positionally, anonymous record fields
sorted, references to other components replaced by their already-computed
hashes, and references within the component replaced by a positional back
reference. Self-recursive declarations — the common case — are therefore fully
name-independent. For components with several members, member order is fixed by
sorting on qualified name, and those names enter the encoding; two isomorphic
mutually recursive groups with different names will not share an address. This
is a known, documented limitation rather than an oversight.

---

## 6. Expressions *(specified, in progress)*

A signature may carry a **body**: a constructive proof written in a small
expression language, which both runs directly and transpiles into every target.

The language is **total** by construction: no recursion, no lambdas, no
unbounded loops. `map_through` is the only iteration form: it maps a
transformation through a constructor — every element of a list, the value of an
optional if it is there, the eventual result of a future — visiting what already
exists and unable to produce more. One form covers every container, because
`list`, `optional`, and `future` are the same idea applied three ways.

Note what `map_through` does *not* offer: there is no way to ask whether an
optional value is present. A body can say what to do with a value if there is
one; it cannot branch on absence and do something else entirely.

The call graph among bodies must be acyclic, checked by topological sort.
Totality is what lets a body be trusted as a proof and lets a transpiled body be
trusted in a language with no such guarantee.

Computations needing real recursion — sorting, for instance — remain
propositions with external implementations. That division is intended.

---

## 7. Environments and implementations

An implementation does not run in a language, it runs in an **environment**: a
browser, a Node process, a Python interpreter, the host process, a WebAssembly
sandbox, something already running elsewhere. The language says what the code is
written in; the environment says where it executes. They are separate because
the same code can be reached more than one way, and none of those ways changes
the proof.

Nothing is discovered. An environment loads a module and the module **registers
as an execution source**, announcing which propositions it can prove and under
what entry name. A manifest is that announcement written down:

```json
{"version": "1.0",
 "environments": [
   {"id": "python", "language": "python",
    "reach": {"k": "subprocess", "command": "python3", "args": ["impls.py"]}}],
 "implementations": [
   {"id": "sort.py_bubble", "proves": "demo::sort", "environment": "python",
    "entry": "bubble_sort", "cost": {"time": "o_n2", "space": "o1"}}]}
```

Relative paths in a manifest resolve against the manifest's own directory, so it
means the same thing wherever it is loaded from.

Complexity is **declared, not measured**. The dispatcher believes the claim; §7.3
is how the claims get checked against each other.

At the dispatch boundary, arguments are checked against the signature's
parameters and the result against its return type. A result that does not match
means the proof is wrong, and an implementation **must not** be retried
elsewhere in that case, however forgiving the caller has asked the dispatcher to
be. A failure to *reach* an environment is a different thing and may be retried.

### 7.1 The subprocess protocol **(frozen)**

One request in, one response out, over the child's standard streams. The parent
closes its writing end; **end of input is the frame delimiter**, so nothing has
to agree on a length prefix or a terminator.

```json
→ {"unitype": "1", "kind": "invoke", "signature": "demo::sort",
   "entry": "bubble_sort", "args": [[3, 1, 2]]}
← {"ok": true, "value": [1, 2, 3]}
```

A failure the child recognizes as its own:

```json
← {"ok": false, "error": {"code": "value_error", "message": "..."}}
```

Arguments are values in the §4 encoding. The response value is read against the
signature's return type by §5's rules, so it becomes a map, an enum, or a tuple
where the type says so.

A child MUST check `unitype` and refuse a version it does not know. A child that
reports a failure this way SHOULD still exit zero: a non-zero exit says the
environment is broken, which is a different claim. Whatever it writes to its
error stream is kept and shown when it exits non-zero or sends something
unreadable.

One process per call is the specified behavior. A long-lived worker is a
compatible extension — the request shape leaves room for an `id` — but is not
part of version 1.

### 7.3 Conformance **(frozen)**

A signature says what shape comes back, not what is in it — `sort` and `reverse`
have the same type. So the proofs of one proposition are also checked against
each other, by running them on the same inputs:

```json
{"version": "1.0",
 "signature": "demo::sort",
 "float_tolerance": 0,
 "vectors": [
   {"name": "backwards", "args": [[3, 2, 1]], "expect": [1, 2, 3]},
   {"name": "a long one", "args": [[9, 3, 7, 1]]}]}
```

Arguments and the expected answer are read against the signature's types by §5's
rules. A vector with no `expect` still checks something: the proofs are held to
each other, which finds a disagreement without anybody having worked out the
right answer first.

Comparison is between **values, not their text**. Two encoders that write a
number differently, or an object with its keys in another order, agree. An
integer and the same number written as a float agree. Floats agree within
`float_tolerance`, which defaults to exact.

A proof that could not be run — no interpreter on this machine — is **skipped**,
not counted as disagreeing. An implementation reporting a failure *is* counted:
a proof that does not answer has not proved anything.

### 7.4 Code generation *(specified, in progress)*

Each target language has a **profile**: how the symbolic types are spelled, how
constructors are written, which identifiers are reserved and how to escape one,
which spellings need bracketing when nested, and a small universe of the
language's own builtin types for the cases the symbolic layer has no exact
counterpart for. A profile also records where precision goes — that a 64-bit
integer spelled as a TypeScript `number` is exact only to 2^53, that a Python
`int` has no width — next to the spelling it qualifies.

A profile is data, so a language this specification never mentions is added by
writing one.

---

## 8. Links *(specified, in progress)*

A link is a directed correspondence from a type in one universe to a type in
another, justified in one of three ways: the two are structurally identical, so
the conversion is a cast; the conversion is a mechanical field mapping, which
every backend can generate; or the conversion is arbitrary and a named signature
is the obligation someone discharged by implementing it.

Links are directed. A round trip is two links, which keeps a lossy conversion
from masquerading as a reversible one. A field mapping that drops source fields
is lossy and must say so.

---

## 9. Reflection *(specified, not yet built)*

A built-in `unitype` universe describes this protocol's own IR. Because
everything serializes canonically, every type is also a value that validates
against `unitype::Type` — so types can be sent through bridges, code generation,
and pipelines like any other data, with no separate reflection mechanism.

---

## Version history

- **1.0** — initial. Schema `1.0`; hash domains `unitype {local,decl,sig,alias}
  v1`; wordlist `bip39-english-v1`.
