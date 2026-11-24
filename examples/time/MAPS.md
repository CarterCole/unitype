# Maps as OWL individuals

Status: design. No universe in `universes/` changes. The types below were
checked in a scratch project outside the repo with the reference `unitype`
binary (`check` passes, and the example individual and its negative cases
validate as stated). They are a proposal for `examples/time/universes/`
once the open points in §7 are settled.

## 1. What the spec and checker allow

These facts decide most of the shape, so they come first.

- **Map keys.** `map<K, V>` takes a key of `string`, `i32`, `i64`, a
  non-boolean literal, or a union of those (PROTOCOL §1.1). The checker
  (`ir/ty.rs`, `is_valid_map_key`) also accepts a `bounded` wrapping a valid key,
  so a restricted string is a valid key. It does **not** accept a named alias as
  a key. Tested: `"key": "Iri"` with `Iri` an alias of a restricted string is
  refused with `owl::Iri cannot be a map key`. An IRI key must be spelled out
  inline, as `{"k":"bounded","of":"str","match":"…"}`.
- **Empty collections.** A map entry is present or absent, so "a property asserted
  at least once" is a list bounded below by one: `{"k":"bounded","of":{"k":"list",…},"len":{"min":1}}`.
  Tested: an empty list is refused with `a length of 0 is not within 1..`.
- **Enums on the wire** are `{"$tag": "Name", "value": …}` (PROTOCOL §4), so a
  tagged literal is self-describing in JSON.
- **Recursive declarations** are allowed (the shape-hash section of the spec
  covers mutually recursive groups). Tested with a recursive `ClassExpr`.
- **Records cannot be keys.** A key is a scalar, so a structured identifier
  (a tuple of values) cannot be a map key. Identity has to be a single IRI, or a
  check over the individual's values (§3.6).

## 2. Decisions at a glance

| OWL construct | Shape in unitype | Why this shape |
|---|---|---|
| Data-property assertion | `data: map<Iri, [Literal; 1..]>` | Open key set; a property can have several values, so the value is a list. |
| Annotation (`rdfs:label`, `rdfs:comment`) | `annotations: map<Iri, [Literal; 1..]>` | Same type as `data`, but a separate field, because the field name carries the role (§3.1). |
| Object-property assertion | `object: map<Iri, [Iri; 1..]>` | The value is another individual, named by its IRI. |
| `rdf:type` (class membership) | `types: [Iri]` | A class is a value, not a property. A map keyed by `rdf:type` would hold one key and one list, which is an indirection with no content. |
| `rdfs:subClassOf` | `SubClassOf { sub: ClassExpr, sup: ClassExpr }` list, at the vocabulary level | The right-hand side is a class expression (intersection, restriction), not a name, so it needs a recursive type, not a key-value entry. |
| `owl:FunctionalProperty` | `Property.functional: bool`, checked: at most one value | A property's characteristic is per property. A map's value type is uniform across keys, so the type cannot say "one value for this key and many for that". |
| `owl:InverseFunctionalProperty` | `Property.inverse_functional: bool`; an identity rule | It decides identity (§3.6), so it yields a `same_as` obligation rather than a key. |
| `owl:inverseOf` | `Property.inverse_of: [Iri]`, stored once | A symmetric axiom about two properties. It is not per individual, so it is not duplicated into each individual's maps. |
| `owl:equivalentProperty` | `Property.equivalent_to: [Iri]` | A vocabulary-level equivalence. In data it means "rewrite the key, keep the value" (§7). |
| `owl:sameAs` | `same_as: [Iri]` field on the individual | An identity assertion. OWL treats it as ABox identity, not as an object property, so it stays out of the `object` map. |
| `owl:differentFrom` | `different_from: [Iri]` field | Also identity (a negative one). Same reasoning. |
| `owl:hasKey` | `KeyDecl { class: Iri, keys: [Iri; 1..] }`, a vocabulary declaration | Identity over property values (§3.6). The keys are property IRIs and are checked against the individual's maps; a key set is never a map key. |
| `foaf:OnlineAccount`, `foaf:accountName` | `foaf:OnlineAccount` is a class; `accountName` is an ordinary data property | FOAF does not make `accountName` identifying by itself. Identity comes only from a `KeyDecl` the ontology declares (§3.6). I did not check FOAF's axioms against the FOAF spec for this design. |
| `schema:identifier`, `schema:alternateName` | ordinary `data`/`annotations` entries | schema.org does not declare these unique. They identify an individual only where our ontology declares a `KeyDecl` or an inverse-functional `Property`. `schema:identifier` can also hold a structured `PropertyValue`, which a literal cannot express (§7). |

## 3. The decisions, in detail

### 3.1 An individual's properties form maps; the field name gives the role

An individual is a struct. Each assertion family is one map field, keyed by
property IRI:

```
Individual {
  iri:          Iri                          -- identity
  types:        [Iri]                        -- rdf:type
  data:         map<Iri, [Literal; 1..]>     -- data-property assertions
  annotations:  map<Iri, [Literal; 1..]>     -- annotation properties
  object:       map<Iri, [Iri; 1..]>         -- object-property assertions
  same_as:      [Iri]                        -- owl:sameAs
  different_from: [Iri]                      -- owl:differentFrom
}
```

`data` and `annotations` have the same type on purpose. OWL gives them different
meanings: a data-property assertion has entailments, an annotation does not. The
type cannot tell two maps of the same type apart, so the struct field name carries
the role. A reader who sees `annotations` knows it is not reasoned over.

### 3.2 Literals are structured by datatype

OWL's literal is a lexical form plus a datatype, but unitype's type system cannot
hold an unbounded integer (PROTOCOL §1.1 excludes arbitrary precision), and
a `f64` would lose `xsd:decimal`. So a literal is an enum with one variant per OWL 2
built-in datatype, each with its own payload, plus an `other` variant for datatypes
the vocabulary declares:

```json
{ "k": "enum", "name": "Literal",
  "variants": [
    { "name": "string",      "payload": "string" },
    { "name": "integer",     "payload": "IntegerText" },
    { "name": "decimal",     "payload": "DecimalText" },
    { "name": "boolean",     "payload": "bool" },
    { "name": "date_time",   "payload": "time::Moment" },
    { "name": "lang_string", "payload": "LangText" },
    { "name": "other",       "payload": "OtherLiteral" } ] }
```

- `integer` is text matching `[+-]?[0-9]+`, not `i64`, so an OWL integer past 2^63 is still representable. Tested: `"1.5"` as an integer is refused by the pattern.
- `date_time` reuses `time::Moment`, so the time universe and the OWL shape agree on what a timestamp is. The pattern checks shape only: `2026-02-30T…` still matches. That is the same caveat `time.json` already states.
- `boolean` is `bool`, so the lexical forms `"1"` and `"0"` of `xsd:boolean` cannot be stored as written. The producer must canonicalize them to `true` or `false` before the value enters the map. That loses the original spelling, recorded in §7.
- `lang_string` carries `text` and a BCP-47-shaped `lang`.
- `other` holds `datatype` (an IRI) and `lexical` (text). It is the escape for custom datatypes, and it is the reason the enum is not closed over every datatype.

The wire form is tagged, so `{"$tag":"integer","value":"1042"}` is the literal `1042`
typed as `xsd:integer`. It is less readable than Turtle, and that is the cost of
being structured.

### 3.3 IRIs are inline restricted strings

An IRI is a string on the wire, restricted by a regular expression in full (PROTOCOL
§1.1, restrictions). The pattern used throughout is an absolute IRI: a scheme, a
colon, then no whitespace or the characters `<>"{}|^`\`.

```json
{ "k": "bounded", "of": "str", "match": "[A-Za-z][A-Za-z0-9+.-]*:[^\\s<>\"{}|^`\\\\]*" }
```

Keys must be written inline (§1). Elsewhere (a field, a list element, an enum
payload) a named alias is fine, and the design uses `Iri` as an alias there.
Checker fix proposed in §7: resolve an alias to its target before the key check,
which removes the duplication without changing the spec.

A closed vocabulary (a fixed set of properties) is a union of literal keys, which the
checker accepts: `map<"https://…#age" | "https://…#name", …>`. Use it where the
vocabulary is closed. It is the most structured a key can be.

### 3.4 Namespace: the key is the full IRI; `universe::name` is authoring only

Data keys are full IRIs. A key never holds `universe::name`, because that is not an
IRI and does not survive export to RDF. A unitype declaration `time::start` names
the IRI `base(time) + "start"`, where `base(time)` is the universe's base IRI. That
gives `https://example.com/time#start`.

The base IRI is not in the spec. A universe would need one more member, which is
metadata and so outside identity (PROTOCOL §5). That is a spec addition, and it is
listed in §7. Until it exists, the example below writes full IRIs directly.

The local name is a unitype identifier (`[A-Za-z_][A-Za-z0-9_]*`), so it is a legal IRI
fragment with no escaping. That is why `duration_ms` keeps its snake case in the IRI.

### 3.5 The key set is open; closure is a check

OWL is open world. An individual may carry assertions the ontology never mentions,
such as vendor annotations, and an absent key means "not asserted", not "false". So
the map type is open: `map<Iri, …>`, with any IRI allowed as a key.

Closure is then a check: a key that no `Property` in the vocabulary declares is a
warning, not a type error. The closed form (§3.3) is available when a schema really
does want a fixed set.

### 3.6 Identity: the IRI is the identity; identifying properties are checks

A map keyed by IRI makes the IRI the identity of the individual. The alternative,
keying an individual by its `hasKey` values, would need a structured key, and
records cannot be map keys (§1). So:

- `iri` is the identity.
- `owl:InverseFunctionalProperty` and `owl:hasKey` do not change the shape. They
  are rules over the individual's values: two individuals whose `KeyDecl.keys`
  have equal values **must** be the same, so the checker emits a `same_as`
  obligation between their IRIs.
- A `same_as` assertion merges two individuals. Their `data` maps then union, and
  a `FunctionalProperty` that had one value may now have two. That is an
  inconsistency, and OWL reports it as one. The policy for reporting it is open (§7).

### 3.7 Cardinality: lists with a lower bound; functional is a check

Every map value is `[T; 1..]`, which says "at least one assertion, if the key is
present". Functional is not a type. A `Property` with `functional: true` is checked
to have at most one value wherever it appears. The type system cannot vary a map's
value by key, so this is the only honest place for it.

### 3.8 Class membership and subclass axioms are not maps

`types: [Iri]` is a list. It is not a map, because class membership is a set of
values, and a map keyed by `rdf:type` has no second key to use.

Subclass axioms are a list of pairs of class expressions:

```json
{ "k": "struct", "name": "SubClassOf",
  "fields": [ { "name": "sub", "ty": "ClassExpr" },
              { "name": "sup", "ty": "ClassExpr" } ] }
```

`ClassExpr` is a recursive enum (named, intersection, some-values-from). It is
checked in the scratch project. A subclass axiom is vocabulary-level, so it lives
next to the `Property` declarations, not inside an individual.

## 4. One concrete individual

The same individual in Turtle and as the map. Every literal is tagged with its
datatype. The `time:` prefix is `https://example.com/time#`, the `ex:` prefix is
`https://example.com/ex#`.

```turtle
ex:release-1-0 a ex:Release ;
  time:start        "2026-11-24T09:00:00Z"^^xsd:dateTime ;
  time:duration_ms  "3600000"^^xsd:integer ;
  ex:buildNumber    "1042"^^xsd:integer ;
  ex:colour         "teal"^^ex:Colour ;
  rdfs:label        "Release 1.0"@en ;
  ex:builtBy        ex:ci-runner-7 .
```

```json
{
  "iri": "https://example.com/ex#release-1-0",
  "types": ["https://example.com/ex#Release"],
  "data": {
    "https://example.com/time#start":        [{"$tag": "date_time", "value": "2026-11-24T09:00:00Z"}],
    "https://example.com/time#duration_ms":  [{"$tag": "integer", "value": "3600000"}],
    "https://example.com/ex#buildNumber":    [{"$tag": "integer", "value": "1042"}],
    "https://example.com/ex#colour":         [{"$tag": "other", "value": {"datatype": "https://example.com/ex#Colour", "lexical": "teal"}}]
  },
  "annotations": {
    "http://www.w3.org/2000/01/rdf-schema#label": [{"$tag": "lang_string", "value": {"text": "Release 1.0", "lang": "en"}}]
  },
  "object": {
    "https://example.com/ex#builtBy": ["https://example.com/ex#ci-runner-7"]
  },
  "same_as": [],
  "different_from": []
}
```

Checked: `validate --type owl::Individual` on this file passes. The negative
case, `"1.5"` as an integer, is refused with
`$.data[…][0].value: "1.5" does not match the pattern "[+-]?[0-9]+"`.

`time:start` and `time:duration_ms` are the two fields of `time::Span` in `time.json`.
Projecting an individual onto `Span` is partial by design: `duration_ms` is `i64[0..]`
in `time.json`, but an OWL integer can be larger than `i64`, so the individual keeps
it as text and the projection is checked at the point of use.

## 5. The validated sketch types

Both universes below were checked in the scratch project. `owl` holds the
individual; `tbox` holds the vocabulary-level declarations. Neither is in
`universes/`. The blocks are trimmed of `doc` strings, so they read shorter than
the tested files; the types and fields are the same. The `time::Moment` reference assumes the `time` universe is present
beside them.

<details>
<summary><code>owl</code> universe (individual and literal)</summary>

```json
{
  "name": "owl",
  "types": [
    { "k": "alias", "name": "Iri", "target": { "k": "bounded", "of": "str", "match": "[A-Za-z][A-Za-z0-9+.-]*:[^\\s<>\"{}|^`\\\\]*" } },
    { "k": "alias", "name": "IntegerText", "target": { "k": "bounded", "of": "str", "match": "[+-]?[0-9]+" } },
    { "k": "alias", "name": "DecimalText", "target": { "k": "bounded", "of": "str", "match": "[+-]?([0-9]+\\.?[0-9]*|\\.[0-9]+)" } },
    { "k": "alias", "name": "LangTag", "target": { "k": "bounded", "of": "str", "match": "[a-zA-Z]{1,8}(-[a-zA-Z0-9]{1,8})*" } },
    { "k": "struct", "name": "LangText", "fields": [ { "name": "text", "ty": "string" }, { "name": "lang", "ty": "LangTag" } ] },
    { "k": "struct", "name": "OtherLiteral", "fields": [ { "name": "datatype", "ty": "Iri" }, { "name": "lexical", "ty": "string" } ] },
    { "k": "enum", "name": "Literal",
      "variants": [
        { "name": "string", "payload": "string" },
        { "name": "integer", "payload": "IntegerText" },
        { "name": "decimal", "payload": "DecimalText" },
        { "name": "boolean", "payload": "bool" },
        { "name": "date_time", "payload": "time::Moment" },
        { "name": "lang_string", "payload": "LangText" },
        { "name": "other", "payload": "OtherLiteral" } ] },
    { "k": "struct", "name": "Individual",
      "fields": [
        { "name": "iri", "ty": "Iri" },
        { "name": "types", "ty": { "k": "list", "of": "Iri" } },
        { "name": "data", "ty": { "k": "map", "key": { "k": "bounded", "of": "str", "match": "[A-Za-z][A-Za-z0-9+.-]*:[^\\s<>\"{}|^`\\\\]*" }, "value": { "k": "bounded", "of": { "k": "list", "of": "Literal" }, "len": { "min": 1 } } } },
        { "name": "annotations", "ty": { "k": "map", "key": { "k": "bounded", "of": "str", "match": "[A-Za-z][A-Za-z0-9+.-]*:[^\\s<>\"{}|^`\\\\]*" }, "value": { "k": "bounded", "of": { "k": "list", "of": "Literal" }, "len": { "min": 1 } } } },
        { "name": "object", "ty": { "k": "map", "key": { "k": "bounded", "of": "str", "match": "[A-Za-z][A-Za-z0-9+.-]*:[^\\s<>\"{}|^`\\\\]*" }, "value": { "k": "bounded", "of": { "k": "list", "of": "Iri" }, "len": { "min": 1 } } } },
        { "name": "same_as", "ty": { "k": "list", "of": "Iri" } },
        { "name": "different_from", "ty": { "k": "list", "of": "Iri" } }
      ] }
  ]
}
```

</details>

<details>
<summary><code>tbox</code> universe (vocabulary declarations)</summary>

```json
{
  "name": "tbox",
  "types": [
    { "k": "enum", "name": "PropertyKind", "variants": [ { "name": "data" }, { "name": "object" }, { "name": "annotation" } ] },
    { "k": "struct", "name": "Property",
      "fields": [
        { "name": "iri", "ty": { "k": "bounded", "of": "str", "match": "[A-Za-z][A-Za-z0-9+.-]*:[^\\s<>\"{}|^`\\\\]*" } },
        { "name": "kind", "ty": "PropertyKind" },
        { "name": "functional", "ty": "bool" },
        { "name": "inverse_functional", "ty": "bool" },
        { "name": "inverse_of", "ty": { "k": "list", "of": { "k": "bounded", "of": "str", "match": "[A-Za-z][A-Za-z0-9+.-]*:[^\\s<>\"{}|^`\\\\]*" } } },
        { "name": "equivalent_to", "ty": { "k": "list", "of": { "k": "bounded", "of": "str", "match": "[A-Za-z][A-Za-z0-9+.-]*:[^\\s<>\"{}|^`\\\\]*" } } }
      ] },
    { "k": "struct", "name": "KeyDecl",
      "fields": [
        { "name": "class", "ty": { "k": "bounded", "of": "str", "match": "[A-Za-z][A-Za-z0-9+.-]*:[^\\s<>\"{}|^`\\\\]*" } },
        { "name": "keys", "ty": { "k": "bounded", "of": { "k": "list", "of": { "k": "bounded", "of": "str", "match": "[A-Za-z][A-Za-z0-9+.-]*:[^\\s<>\"{}|^`\\\\]*" } }, "len": { "min": 1 } } }
      ] },
    { "k": "struct", "name": "Intersection", "fields": [ { "name": "parts", "ty": { "k": "list", "of": "ClassExpr" } } ] },
    { "k": "struct", "name": "SomeValuesFrom",
      "fields": [
        { "name": "on", "ty": { "k": "bounded", "of": "str", "match": "[A-Za-z][A-Za-z0-9+.-]*:[^\\s<>\"{}|^`\\\\]*" } },
        { "name": "filler", "ty": "ClassExpr" }
      ] },
    { "k": "alias", "name": "NamedClass", "target": { "k": "bounded", "of": "str", "match": "[A-Za-z][A-Za-z0-9+.-]*:[^\\s<>\"{}|^`\\\\]*" } },
    { "k": "enum", "name": "ClassExpr",
      "variants": [
        { "name": "named", "payload": "NamedClass" },
        { "name": "intersection", "payload": "Intersection" },
        { "name": "some_values_from", "payload": "SomeValuesFrom" } ] },
    { "k": "struct", "name": "SubClassOf", "fields": [ { "name": "sub", "ty": "ClassExpr" }, { "name": "sup", "ty": "ClassExpr" } ] }
  ]
}
```

</details>

Checked in the scratch project: `check` passes for both universes. `Property`
with `functional: true` on `ex:buildNumber` validates. A `KeyDecl` with two keys
validates. A `SubClassOf` whose superclass is an intersection containing a restriction,
which is recursive, validates.

## 6. What the decisions rule out

- A map keyed by `rdf:type`, or a map from class to superclasses. Both lose the class expression on the right-hand side.
- A single `Value` union (IRI or literal) across all assertions. It works on the wire, but it merges the object and data roles into one field, and a bare IRI string could then be either.
- `i64` for an OWL integer. It cannot hold the full range of `xsd:integer`.
- A structured identity key. Records cannot be map keys (§1), so `hasKey` is checked, not indexed.
- Keys written as `universe::name`. They are not IRIs, and they do not survive RDF export.

## 7. Open

1. **Alias keys.** The checker refuses an alias as a map key (`ir/ty.rs`, `is_valid_map_key` returns false for `Ref`), though the spec's wording is only "string" and aliases are its normal naming tool. Proposed fix: resolve the alias before the key check. Until then, keys are inline. This needs a decision by the spec's owner.
2. **Universe base IRI.** A universe needs a base IRI for `universe::name` to become an IRI. The field is metadata, so it should not change identity, but it is a spec addition and I have not written it.
3. **Lexical checks are shape only.** `date_time` matches a pattern, so `2026-02-30` passes. `boolean` canonicalizes `"1"` to `true`, a loss. `decimal` has no range. These are the same limits `time.json` already states, plus the boolean loss.
4. **Equivalence over maps.** `owl:equivalentProperty` means "rewrite the key and keep the value". PROTOCOL §8 defines equivalence over struct fields and does not define it over map entries. Proposed: an equivalence over maps, pairing keys, with values converting exactly.
5. **Sets.** `types` and `same_as` are sets in OWL and lists here. Unitype has no set type, and list order is not semantic. Convention: sort by IRI on write, and the checker may enforce that.
6. **Identity merges.** When `same_as` merges two individuals and a functional property ends with two values, the policy is open: reject, keep both, or report. I recommend reporting as an inconsistency, as OWL does.
7. **Structured `schema:identifier`.** schema.org allows a `PropertyValue` as a value, which is an object with its own properties. A `Literal` cannot hold it. Proposed: a variant `structured` whose payload is a map.
8. **FOAF.** The claim that `foaf:accountName` is not identifying alone is from memory, not from the FOAF spec. Check the FOAF axioms before a `KeyDecl` relies on them.
9. **Open world.** An absent key means "not asserted". The checker needs to be told which checks assume closure (cardinality, functional) and which do not (presence), or it will read absence as false.
10. **Subclass axioms beyond intersection and restriction.** OWL also has union, complement, and enumerated classes. `ClassExpr` covers three of them. The rest is a straightforward extension, not checked here.
