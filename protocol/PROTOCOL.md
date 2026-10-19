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

**Variance.** `optional`, `list`, `future`, both halves of a `map`, `tuple`
items, `record` fields, `union` members, and a function's result are
covariant; a function's parameters are contravariant. A declared generic's
arguments vary the way the declaration uses them. Nothing declares this: each
parameter is covariant if it occurs only in covariant positions, contravariant
if only in contravariant ones, invariant if in both, and unused if nowhere, in
which case any argument fits any other. A struct or interface counts its
fields, inherited ones included, and the arguments it passes to its
supertypes; an enum counts its payloads; an alias counts its target. An
occurrence inside another generic composes with that generic's variance, so
declarations that mention each other are solved together, from unused upward
until nothing changes. This is sound because values are immutable data, and it
changes no serialization and no address.

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
make_variant call let if map_through prim fold case match`.

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
unbounded loops. Every form either builds a value, names one, or visits what
already exists a bounded number of times.

**Iteration.** Two forms visit a list, and neither can produce more to visit.

- `map_through` maps a transformation through a constructor — every element of
  a list, the value of an optional if it is there, the eventual result of a
  future. One form covers every container, because `list`, `optional`, and
  `future` are the same idea applied three ways.
- `fold` carries a value through a list from the left:
  `{"k":"fold","list":…,"init":…,"acc":"total","item":"x","body":…}`. The body
  sees the value carried so far as `acc` and the next element as `item`, and
  what it produces is carried on. It cannot name the fold, the rest of the list,
  or anything that would let it go round again: this is primitive recursion over
  a finite value. The carried value keeps one type throughout — the optional
  `ty`, or else the type of `init` with literals widened to their base types —
  and each step MUST produce something of that type.

**Elimination.** Two forms take a value apart, and each runs exactly one arm.

- `case` says what to do whether or not a value is there:
  `{"k":"case","value":…,"var":"n","some":…,"none":…}`. `var` is bound to the
  value in `some` only. The result is the join of the two arms.
- `match` says what to do with each variant of an enum:
  `{"k":"match","ty":"Size","value":…,"arms":[{"tag":"large","var":"n","body":…}]}`.
  There MUST be exactly one arm per variant. An arm's `var`, when present, is
  bound to the payload, and MUST be absent for a variant that carries nothing.
  The result is the join of the arms.

**Operations.** `{"k":"prim","op":…,"args":[…]}` applies one of a closed set of
operations every target spells in place:

| `op` | operands | result |
|---|---|---|
| `eq` `ne` | two values, one a subtype of the other | `bool` |
| `lt` `le` `gt` `ge` | two integers, two floats, or two strings | `bool` |
| `add` `sub` `mul` | two integers, or two floats | `i64`, or `f64` |
| `neg` | one integer, or one float | `i64`, or `f64` |
| `and` `or` | two booleans; the second is evaluated only if it decides | `bool` |
| `not` | one boolean | `bool` |
| `len` | one list | `i64` |

Equality is between values, not their text, in the sense of §7.4. Strings are
ordered by code point. Integers and floats are not mixed: `x + 1` on a float is
an error in the body, not a widening, because each target would widen
differently. An integer result outside `i64`, or a float result that is not
finite, is a failure of the body rather than a wrapped or infinite value. There
is no division, because a body that divides by zero would not answer.

**Constants.** A `lit` of a boolean, integer, or string is typed as the literal
type of that value, so a body can compare a discriminator with the one value it
may hold. Other constants are typed as §4 infers them.

The call graph among bodies must be acyclic, checked by topological sort.
Totality is what lets a body be trusted as a proof and lets a transpiled body be
trusted in a language with no such guarantee.

Computations needing real recursion — sorting, for instance — remain
propositions with external implementations. That division is intended. What a
fold *can* do is check that one happened: whether a list is in order is a walk
along it remembering the last element.

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

Complexity is **declared, not measured**. The dispatcher believes the claim; §7.4
is how the claims get checked against each other.

At the dispatch boundary, arguments are checked against the signature's
parameters and the result against its return type. A result that does not match
means the proof is wrong, and an implementation **must not** be retried
elsewhere in that case, however forgiving the caller has asked the dispatcher to
be. A failure to *reach* an environment is a different thing and may be retried.

### 7.1 The subprocess protocol **(frozen)**

One request in, one response out, over the child's standard streams. **Each
message is one line**, terminated by a newline; end of input also ends the
stream. A child that reads lines and a child that reads to the end are both
correct, so nothing has to agree on a length prefix.

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

One process per call is the default, and the only behavior a child has to
support. §7.2 is how to keep one alive.

### 7.2 Workers **(frozen)**

An environment may declare `"persistent": true`, and then one process serves
every call to it instead of one process per call. That saves what starting an
interpreter costs, which for Python is around twenty milliseconds against
microseconds of actual work.

The messages are the ones §7.1 specifies. Only the framing matters: with the
stream staying open, end of input can no longer mean end of message, so the
newline does. A child that reads lines needs no mode of its own — sent one
message and then a closed stream, its loop runs once and the process exits,
which is exactly §7.1.

Both sides MUST flush after each message, or a parent waiting on the next line
waits on a buffer that never fills.

A parent MUST NOT reuse a worker after an exchange goes wrong — a failure to
read, a timeout, or a closed stream — because the stream's position is then
unknown, and a late answer would be read as the next call's result. It MUST kill
a worker it abandons. A failure the worker *reports* is not such a case: the
worker is fine and knows where it is, and may be used again.

Calls to one worker are serialized, since two messages down one pipe have no way
to be told apart. An implementation wanting concurrency runs more than one
worker.

**What this costs.** State survives between calls. That is the whole saving and
the whole risk, so it is opt-in: a module that holds nothing between calls says
so by not asking.

### 7.3 WebAssembly **(frozen)**

Every other environment is trusted. A function compiled into the host can do
whatever the host can; a subprocess can read the same files. That is usually the
right trade, and it is the wrong one for a proof that arrived from somewhere.

A WebAssembly guest gets linear memory and nothing else: no files, no sockets,
no clock, no way to reach the host except by returning. The messages are §7.1's,
unchanged. Only the carriage differs — instead of a pipe, the request is written
into the guest's own memory and the answer is read back out of it.

A guest MUST export exactly these three things:

```wat
(memory (export "memory") …)
(func (export "unitype_alloc") (param $len i32) (result i32))
(func (export "unitype_invoke") (param $ptr i32) (param $len i32) (result i64))
```

The exchange:

1. The host calls `unitype_alloc(len)` and gets back an offset with room for
   `len` bytes.
2. The host writes the request there — the same JSON line as §7.1, without the
   newline, which has nothing left to frame.
3. The host calls `unitype_invoke(ptr, len)`.
4. The guest returns where its answer is, packed as `ptr << 32 | len`, because
   one number is all a WebAssembly function can return.
5. The host reads that many bytes and reads them as §7.1's response.

The guest MUST keep the answer's bytes valid until `unitype_invoke` returns,
which is all it has to do: the instance is discarded afterwards, so there is
nothing to free and nobody to free it. A guest compiled from a language with an
allocator SHOULD simply leak both blocks.

A host MUST check the returned `(ptr, len)` against the size of the guest's
memory before reading, and MUST treat a pair that does not fit as a protocol
failure naming the guest. This is the one place a guest could reach outside its
own world, and it is the host's job to see that it cannot.

**Instances are not reused.** A module MAY be compiled once and kept — compiling
is expensive and instantiating is not — but each call MUST get a fresh instance.
Nothing survives from one call to the next, for the same reason as §7.1's one
process per call: a proof cannot get a later call wrong by remembering an
earlier one. §7.2's opt-in does not apply here; there is no worker to keep.

**Budget, not timeout.** There is no operating system process to kill, so a
guest MUST run under a bound on the work it may do, and a host MUST stop one
that exceeds it. Exceeding the budget says the sandbox worked, not that the
proof is wrong, so it is an environmental failure and MAY be retried elsewhere —
unlike a proof that returns the wrong thing, which never is.

### 7.4 Conformance **(frozen)**

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

### 7.5 Code generation *(specified, in progress)*

Each target language has a **profile**: how the symbolic types are spelled, how
constructors are written, which identifiers are reserved and how to escape one,
which spellings need bracketing when nested, and a small universe of the
language's own builtin types for the cases the symbolic layer has no exact
counterpart for. A profile also records where precision goes — that a 64-bit
integer spelled as a TypeScript `number` is exact only to 2^53, that a Python
`int` has no width — next to the spelling it qualifies.

A profile is data, so a language this specification never mentions is added by
writing one.

### 7.6 Free theorems *(specified, in progress)*

A generic proposition says more than a vector file can. A proof of
`reverse<T> : [T] -> [T]` is given no way to look at a `T` — no comparison, no
field, nothing but the shape around it — so whatever it does, it does the same
way at every `T`. That is a theorem about every proof of the proposition, and
it follows from the signature alone (Reynolds's abstraction theorem; Wadler,
*Theorems for free!*). Two consequences are checkable by running a proof, and
neither needs anyone to say what the right answer is:

- **Conservation.** Every `T` in the result was one of the `T`s in the
  arguments.
- **Naturality.** For any relabelling `g` of the `T`s, `f(map g xs) = map g (f
  xs)`.

The harness instantiates each parameter at `i64`, fills every parameter
position with a distinct token, and runs the proof. Which token landed where
is read off the result by walking the declared return type, which is what makes
conservation checkable and predicts the answer at every other type. The proof
is then run at `str` with the tokens made distinct strings, at `bool` with them
all made equal, and at `i64` with them collapsed onto two values, and each
answer must be the first one relabelled the same way. One type is enough to
test a parametric proof at; the other three are there to catch a proof that is
not parametric — one that sorts when it sees integers, compares two elements
and branches, or only works at the type its author had in mind.

Trials are deterministic: lists and maps of sizes 0, 1, 2, 3 and 5; optionals
alternately absent and present; enums cycling through their variants. A trial
where a proof could not be run is skipped, not counted; a proof that fails at a
relabelled type has broken naturality, since a proof of a claim about every type
that only works at one has not proved the claim.

What is not covered, and is reported as such rather than checked wrongly: a
**bounded** parameter can be looked at through its bound, so the theorem weakens
to naturality over bound-respecting relabellings; a parameter under an
**untagged union** cannot be found in a value; and a parameter type that cannot
be filled from nothing — a function, an interface — cannot be tried.

A call to a generic proposition is made *at* type arguments (§7's boundary
checks run against the instantiated signature), and the proof is not told
which: a proof of a generic proposition is one that did not need to know.

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

## 9. Reflection

A built-in universe named `unitype` declares this protocol's own representation:
what a type is, what a declaration is, what a universe is. An implementation
SHOULD ship it.

Nothing new is needed to make it work. Types, declarations, and universes
already serialize canonically (§2), so each of them was always a value; the
universe just says which value. Once it is loaded, a declaration can be
validated, addressed, named in three words, moved through a bridge, generated as
code, and passed to a proposition — because it is data, and all of that is what
this protocol does with data.

`Type` is declared as a union of one struct per variant, each pinned by a
literal `k`, which is exactly the wire form of §2. That is what makes the
description checkable rather than merely descriptive: an implementation MUST
verify its own declarations against it, so the description cannot drift from
what it describes. Adding a variant without adding it here has to fail.

---

## Version history

- **1.0** — initial. Schema `1.0`; hash domains `unitype {local,decl,sig,alias}
  v1`; wordlist `bip39-english-v1`.
