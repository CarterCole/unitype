# Graphs

This is a design. Nothing in the checker reads graphs yet, and nothing under
`universes/` changes. It sits beside `time`, `space` and `actor` in this
project, and it does not touch `task.json`, the state machine the parent session
is prototyping.

The design has two layers. The first is a generic graph: named nodes and
labelled relationships between them, directed or not, rooted or not. The second
is a plan graph, which is a generic graph whose nodes are plans, tasks and
actors, and whose relationships say what feeds what.

## The graph

A graph is nodes and the edges between them. Two properties are declared, not
inferred:

- **Directed or not.** In a directed graph an edge runs from one node to
  another, so `a → b` and `b → a` are different edges. In an undirected graph an
  edge says the two nodes are related, and holds both ways.
- **Rooted or not.** A graph with a `root` node says how the root relates to the
  rest. In a directed graph, `root_reach` picks the direction. `from` means the
  root reaches every node, as a folder tree does. `to` means every node reaches
  the root, as a set of tasks feeding one result does. In an undirected graph
  the root must be connected to every node, and no direction is needed.

Each edge carries a `rel`, which says what the relationship is, and an optional
`label` for anything else it must name. Relationship names are data: a layer
says which ones it uses.

```json
{
  "name": "graph",
  "doc": "A graph of named nodes and labelled relationships between them. A graph is directed or not. A root, if it has one, is a node that every other node is reached from, or reaches.",
  "types": [
    { "k": "enum", "name": "Reach", "doc": "Which way a root's reach runs along a directed graph.", "variants": [
      { "name": "from", "doc": "The root reaches every node, along the edges." },
      { "name": "to", "doc": "Every node reaches the root, along the edges." }
    ]},
    { "k": "struct", "name": "Edge", "doc": "A relationship from one node to another. In an undirected graph the direction is not meant, and the edge holds both ways.", "fields": [
      { "name": "from", "ty": "str" },
      { "name": "to", "ty": "str" },
      { "name": "rel", "doc": "What the relationship is, such as contains or feeds.", "ty": "str" },
      { "name": "label", "doc": "Anything else the relationship needs to name, such as a parameter.", "ty": "str?" }
    ]},
    { "k": "struct", "name": "Graph", "doc": "Nodes, the edges between them, and whether the edges run one way. A root is optional. A directed graph with a root says which way its reach runs; an undirected one needs no direction, since its reach runs both ways.", "fields": [
      { "name": "name", "ty": "str" },
      { "name": "directed", "ty": "bool" },
      { "name": "nodes", "ty": "[str]" },
      { "name": "edges", "ty": "[Edge]" },
      { "name": "root", "ty": "str?" },
      { "name": "root_reach", "doc": "Only for a directed graph with a root.", "ty": "Reach?" }
    ]}
  ]
}
```

Two examples. A directed tree, rooted from its root:

```json
{
  "name": "repo_tree",
  "directed": true,
  "nodes": ["root", "docs", "src", "readme"],
  "edges": [
    { "from": "root", "to": "docs", "rel": "contains" },
    { "from": "root", "to": "src", "rel": "contains" },
    { "from": "root", "to": "readme", "rel": "contains" }
  ],
  "root": "root",
  "root_reach": { "$tag": "from" }
}
```

An undirected relationship, rooted at one of its members:

```json
{
  "name": "collaborators",
  "directed": false,
  "nodes": ["ana", "ben", "cal"],
  "edges": [
    { "from": "ana", "to": "ben", "rel": "works_with" },
    { "from": "ben", "to": "cal", "rel": "works_with" }
  ],
  "root": "ana"
}
```

The shape is what the type can say. Four rules are not expressible as types,
and the graph pass checks them:

1. Node names are unique, both ends of every edge are nodes, and no edge appears
   twice. In an undirected graph `a–b` and `b–a` are the same edge.
2. `root`, if present, is a node.
3. `root_reach` is present exactly when the graph is directed and has a root. The
   type allows it on an undirected graph; that is refused by this rule.
4. Rootedness. For `from`, every node is reached from the root along the edges.
   For `to`, every node reaches the root. For an undirected graph, every node is
   connected to the root.

Acyclicity is not a generic rule. A layer that needs it asks for it (see the plan
layer, and Q10).

## The plan layer

A plan graph is a directed graph whose nodes are plans, tasks and actors. It is
rooted `to` its result, so every plan, task and actor reaches the final result.
A plan that nothing consumes, and is not the result, would make a second root,
and is refused as a plan that does not reach the root.

The layer uses three relationships:

- `feeds`, from a plan to a plan. The future of the first is an input of the
  second, and `label` names the parameter it fills.
- `realizes`, from a task to a plan. The task is the occurrence that delivers the
  plan's future.
- `performs`, from an actor to a task. The actor carries the task out.

### Decisions

1. **A plan is a signature.** A `functions` entry is already a proposition, and
   a future `ret` with a `wait` window is already the committed future. No new
   type kind and no protocol change.
2. **Feeds name their producer.** Names are not identity, so two plans that
   both return `future<Route>[wait ..5000]` cannot be told apart by type. A feed
   names the plan it comes from, and the type rule checks that the producer's
   future fits the input.
3. **A parameter's window is the consumer's input budget.** `fly` takes
   `future<Route>[wait ..5000]`: the route must arrive by 5000 ms. Whether a
   producer fits that budget is the subtype rule that already exists for
   windowed futures (checked below). No separate notion of dependency is needed.
4. **There is no step field.** The time a plan adds is implied: its output's
   max minus the largest max among its inputs. A plan with inputs must end no
   sooner than they do (rule S1).
5. **Tasks and actors are nodes; relationships are edges.** A task realizes one
   plan and is performed by one actor. A task never feeds anything: a plan's
   inputs are its parameters, and its task inherits them. Every window stays on
   a plan.
6. **Every plan in a graph is called at one origin**, a `time::Moment`. Windows
   count from it, which is the protocol's "from the call that produced the
   future" with every call made at the origin. Sequenced plans, called when an
   input arrives, are open (Q1).
7. **A plan graph is acyclic over its `feeds`.** Cycles are refused by a
   topological sort, as the call graph among bodies already is (protocol §6). The
   single-successor walk in `check/refine.rs` will not do, since a plan can have
   several inputs.
8. **A plan graph is a value, validated like any other data.** The signatures
   live in a domain universe. The graph is a JSON document of type
   `plan::PlanGraph`, which contains a `graph::Graph`. Where graphs are declared
   is open (Q5).

### Declaration

Three pieces, each of which passes the checker as it stands. The text is checked
in a scratch copy of this project (see Verification).

**Plans: signatures in a domain universe.** A plan is a signature. Its
parameters are the futures it consumes, each with the window its input must
arrive in. Its return is the future it commits to. `T?` is the shorthand for an
optional; `optional<T>` is refused.

```json
{
  "name": "survey",
  "types": [
    { "k": "struct", "name": "Forecast", "fields": [
      { "name": "wind_ms", "ty": "f64" }, { "name": "rain_mm", "ty": "f64" } ]},
    { "k": "struct", "name": "Route", "fields": [
      { "name": "waypoints", "ty": "i64[0..]" } ]},
    { "k": "struct", "name": "Imagery", "fields": [
      { "name": "frames", "ty": "i64[0..]" } ]},
    { "k": "struct", "name": "Orthomosaic", "fields": [
      { "name": "pixels", "ty": "i64[0..]" } ]},
    { "k": "struct", "name": "Report", "fields": [
      { "name": "summary", "ty": "str" } ]}
  ],
  "functions": [
    { "name": "forecast", "params": [],
      "ret": "future<Forecast>[wait ..2000]" },
    { "name": "route",
      "params": [{ "name": "forecast", "ty": "future<Forecast>[wait ..2000]" }],
      "ret": "future<Route>[wait ..5000]" },
    { "name": "fly",
      "params": [{ "name": "route", "ty": "future<Route>[wait ..5000]" }],
      "ret": "future<Imagery>[wait ..605000]" },
    { "name": "stitch",
      "params": [{ "name": "fly", "ty": "future<Imagery>[wait ..605000]" }],
      "ret": "future<Orthomosaic>[wait ..725000]" },
    { "name": "report",
      "params": [
        { "name": "stitch", "ty": "future<Orthomosaic>[wait ..725000]" },
        { "name": "forecast", "ty": "future<Forecast>[wait ..2000]" } ],
      "ret": "future<Report>[wait ..730000]" }
  ]
}
```

**The plan vocabulary.** A plan graph names its plans, tasks and actors, and
wraps the generic graph that relates them. The relationships themselves are
edges in that graph.

```json
{
  "name": "plan",
  "doc": "A plan graph: the plans, tasks and actors of one graph, the generic graph that relates them, and the origin every plan is called at.",
  "types": [
    { "k": "struct", "name": "PlanGraph", "doc": "A directed graph of plans, tasks and actors, rooted to its result. Edges are feeds, realizes and performs.", "fields": [
      { "name": "universe", "doc": "The universe whose signatures are the plans.", "ty": "str" },
      { "name": "origin", "doc": "The moment every plan is called.", "ty": "time::Moment" },
      { "name": "plans", "ty": "[str]" },
      { "name": "tasks", "ty": "[str]" },
      { "name": "actors", "ty": "map<str, actor::Actor>" },
      { "name": "existence", "doc": "Where each task has run. Absent until it runs.", "ty": "map<str, time::Existence>?" },
      { "name": "graph", "ty": "graph::Graph" }
    ]}
  ]
}
```

**One graph: `field_survey`.** One orchard, one drone, and two computers. Each
plan has one task. The actors are typed in the `actor` universe, so `Dimensions`
and `Existence` are the real ones.

```json
{
  "universe": "survey",
  "origin": "2026-06-01T06:00:00Z",
  "plans": ["forecast", "route", "fly", "stitch", "report"],
  "tasks": ["fetch_forecast", "plan_route", "fly_field", "stitch_images", "write_report"],
  "actors": {
    "drone": {
      "kind": { "$tag": "computer" },
      "existence": { "$tag": "span", "value": { "start": "2026-05-01T00:00:00Z", "duration_ms": 31536000000 } },
      "size": { "$tag": "three", "value": { "width": 1.2, "height": 0.5, "depth": 1.2 } }
    },
    "mapper": {
      "kind": { "$tag": "ai" },
      "existence": { "$tag": "span", "value": { "start": "2026-01-01T00:00:00Z", "duration_ms": 31536000000 } },
      "size": { "$tag": "three", "value": { "width": 0.6, "height": 2.0, "depth": 1.0 } }
    },
    "planner": {
      "kind": { "$tag": "computer" },
      "existence": { "$tag": "span", "value": { "start": "2026-01-01T00:00:00Z", "duration_ms": 31536000000 } },
      "size": { "$tag": "three", "value": { "width": 0.32, "height": 0.02, "depth": 0.22 } }
    }
  },
  "graph": {
    "name": "field_survey",
    "directed": true,
    "nodes": ["forecast", "route", "fly", "stitch", "report",
              "fetch_forecast", "plan_route", "fly_field", "stitch_images", "write_report",
              "drone", "mapper", "planner"],
    "edges": [
      { "from": "forecast", "to": "route", "rel": "feeds", "label": "forecast" },
      { "from": "route", "to": "fly", "rel": "feeds", "label": "route" },
      { "from": "fly", "to": "stitch", "rel": "feeds", "label": "fly" },
      { "from": "stitch", "to": "report", "rel": "feeds", "label": "stitch" },
      { "from": "forecast", "to": "report", "rel": "feeds", "label": "forecast" },
      { "from": "fetch_forecast", "to": "forecast", "rel": "realizes" },
      { "from": "plan_route", "to": "route", "rel": "realizes" },
      { "from": "fly_field", "to": "fly", "rel": "realizes" },
      { "from": "stitch_images", "to": "stitch", "rel": "realizes" },
      { "from": "write_report", "to": "report", "rel": "realizes" },
      { "from": "planner", "to": "fetch_forecast", "rel": "performs" },
      { "from": "planner", "to": "plan_route", "rel": "performs" },
      { "from": "drone", "to": "fly_field", "rel": "performs" },
      { "from": "mapper", "to": "stitch_images", "rel": "performs" },
      { "from": "planner", "to": "write_report", "rel": "performs" }
    ],
    "root": "report",
    "root_reach": { "$tag": "to" }
  }
}
```

The windows in that graph:

| plan | result | consumes, input budget | commits | step | realized by |
|---|---|---|---|---|---|
| `forecast` | `Forecast` | none (source) | `..2000` | none | `fetch_forecast` by `planner` |
| `route` | `Route` | `forecast ..2000` | `..5000` | 3000 | `plan_route` by `planner` |
| `fly` | `Imagery` | `route ..5000` | `..605000` | 600000 | `fly_field` by `drone` |
| `stitch` | `Orthomosaic` | `fly ..605000` | `..725000` | 120000 | `stitch_images` by `mapper` |
| `report` | `Report` | `stitch ..725000`, `forecast ..2000` | `..730000` | 5000 | `write_report` by `planner` |

"Step" is the commitment's max minus the largest input budget. It is not written
anywhere; it is what the windows say.

## Checks

Run in this order. The first failure is reported, because the later rules assume
the earlier ones hold.

### 1. Graph

The generic rules above: unique nodes, edges that join nodes, no repeated edge,
`root` is a node, `root_reach` only on a directed rooted graph, and rootedness in
the direction given.

### 2. Plan well-formedness

- The nodes are exactly the plans, the tasks and the actor keys, and those three
  sets are disjoint.
- Each `feeds` edge joins two plans, and its `label` names a parameter the
  consumer declares. Each parameter is fed exactly once.
- Each `realizes` edge joins a task to a plan. Each task realizes exactly one
  plan, and each plan is realized by at most one task. A plan with no task is an
  external commitment (Q8).
- Each `performs` edge joins an actor to a task, and each task has exactly one
  performer.
- Every window has a max, on a commitment and on every input. An open max cannot
  be passed on downstream, so it is refused.
- The root is a plan.

### 3. Acyclic

Kahn's algorithm over the `feeds` edges. On failure, one cycle is reported by
walking back-edges, as `RefinementCycle` reports a loop of refinements. A plan
that feeds itself is a cycle.

The window rules below cannot catch a cycle, so this check must run on its own.
A loop whose windows are all equal satisfies every inequality in §4. See the
third refusal in the worked example.

### 4. Windows

For a feed `p → q` through parameter `x` of `q`:

- **G1 (fit).** `p`'s commitment fits `q`'s input `x`. This is the subtype rule
  that already exists: `x.min ≤ p.min` and `p.max ≤ x.max`. A narrower window
  refines a wider one, a wider one is refused, and a plain future with no window
  fits no windowed one (all three checked, see Verification).
- **S1 (outputs after inputs).** For a plan with inputs, its commitment's max is
  at least the largest max of its parameters. A plan that waits for all its inputs
  cannot promise its result before the last of them is due. The gap is the time
  the plan adds.

Lower bounds: a consumer states a minimum on an input only if it cannot accept
early input. Otherwise its parameter is `..B` and no lower bound is checked. A
plan can always wait, so a lower bound on a commitment is never infeasible, and
S1 has no lower-bound counterpart.

Why two local rules are enough: with G1 and S1 on every feed, the max never falls
along a path, and each plan's gap is added to the one before it. The root's max
therefore covers the whole graph.

Scope: v0 covers plans that wait for every input (joins). A plan that returns as
soon as any one input arrives needs its own rule (Q4).

### 5. Tasks

At run, once the task's existence is recorded:

- **Delivery in window.** Delivery is the instant the existence ends: `at`, or
  `start + duration_ms` for a span. `delivered − origin` must lie in the plan's
  window. This is the restriction check that already runs where a value crosses a
  boundary. A host measures it, because a guest has no clock.
- **Causality.** A task's existence starts no earlier than the delivery of each
  of its plan's inputs.
- **Actor existence.** A task's existence lies within its performer's existence.
  This is the one place the `actor` and `time` universes meet inside a graph.

### 6. A miss is where replanning starts

A future that arrives outside its window misses. A miss is not retried: plans
have no alternative proofs inside the graph, so the call fails and names the end
it missed. The host then re-runs the window checks on the missed plan's
downstream subgraph, with the actual delivery in place of the missed window.
Downstream plans that still fit keep running. The first one that does not is
where a replanner has to act.

## Worked example

The windows from the table above, with the runs as they went, in milliseconds
from the origin at `06:00:00Z`:

| plan | delivered | window | |
|---|---|---|---|
| `forecast` | 1200 | `..2000` | within |
| `route` | 4100 | `..5000` | within |
| `fly` | 604300 | `..605000` | within (flight of 600000 from 4300) |
| `stitch` | 714300 | `..725000` | within (110000 of its 120000) |
| `report` | 718300 | `..730000` | within (4000 of its 5000) |

**A miss.** Suppose `route` delivers at 5300. It misses by 300 ms, and the host
records the delivery. Downstream, `fly` was given an input budget of 5000, and its
input arrived at 5300. Re-running the checks with that fact fixed:

- `fly` takes 600000 from 5300, so it ends at 605300, past its 605000 window. The
  check fails at `fly`, and the replanner has three options: move `fly`'s window
  to `..605300` (which moves `stitch` and `report` too, and the check says which
  of them still fit), replace `fly_field` with a faster flight, or stop.

**Refusals**, as the checker would word them. The messages are illustrative; the
wording follows the checker's existing ones.

1. *Route takes longer than its window allows.* Set `route`'s commitment to
   `..5500` (a 3500 step). Then:
   `feed route → fly: future<Route>[wait ..5500] is not usable where
   future<Route>[wait ..5000] is expected`.
2. *A plan that finishes before its input is due.* Set `fly`'s commitment to
   `..4000`. Then: `survey::fly promises ..4000, but its input route is due by
   ..5000`.
3. *A loop, with windows that all agree.* Make `a` consume `b` and `b` consume `a`,
   each with a `..5000` commitment and a `..5000` input budget. Every window rule
   passes, so the acyclic rule alone refuses it: `cycle: a → b → a`.

## OWL mapping

An OWL ontology can hold a graph as data, but it cannot check one. This section
says which parts map, which do not, and what the mapping needs first.

It uses the `owl::Individual` type from `examples/time/MAPS.md`, which is on
`main` at 8d83bd0 and not on this branch. The IRIs below are written in full
because the universe base IRI (MAPS §7.2) is still open.

### Mapping

| Design | OWL | Notes |
|---|---|---|
| Node | Named individual, IRI = base + node name | Node names must be IRI fragments (Q13). |
| Plan, task, actor | `rdf:type` of the node: `plan:Plan`, `plan:Task`, `actor:Actor` | The three classes are pairwise disjoint. |
| Unlabelled edge `a –rel→ b` | Object-property assertion `a rel b`, in `a`'s `object` map | Exact when `(a, rel, b)` is unique. |
| Labelled edge | Reified edge individual (below) | A triple has no slot for a label. |
| `rel` | Object property in the layer's vocabulary; on a reified edge, the edge's type, a subclass of `graph:Edge` | |
| `label` | `graph:label` data property on the reified edge | |
| Directed graph | Object property, which is OWL's default | |
| Undirected graph | Relation that is its own inverse (`inverse_of` contains itself) | OWL 2 defines a symmetric property this way. Direction lives on the property (Q12). |
| `root` | No OWL construct; a `graph:root` object property on a graph individual | Rootedness is a check, below. |
| `root_reach` | Direction of a transitive `reaches`, a super-property of `rel` | Names a direction; asserts nothing. |

The labelled `feeds` edge from `forecast` to `route` in `field_survey`, as an
individual:

```json
{
  "iri": "https://example.com/plan#feeds-forecast-route",
  "types": ["https://example.com/plan#feeds"],
  "data": { "https://example.com/graph#label": [{ "$tag": "string", "value": "forecast" }] },
  "annotations": {},
  "object": {
    "https://example.com/graph#from": ["https://example.com/survey#forecast"],
    "https://example.com/graph#to": ["https://example.com/survey#route"]
  },
  "same_as": [],
  "different_from": []
}
```

The direct assertion `forecast feeds route` is kept only where no other edge
shares its triple. Two edges from one producer to one consumer with different
labels are one triple in OWL, which happens when a consumer takes two parameters
from the same plan. The reified edge keeps both.

### Endpoints: refusal by inconsistency

Domain and range, with disjoint classes, make the plan layer's endpoint rules
into refusals that a reasoner reports:

- `feeds`: domain and range `plan:Plan`.
- `realizes`: domain `plan:Task`, range `plan:Plan`.
- `performs`: domain `actor:Actor`, range `plan:Task`.

A task used as a feed endpoint then types as both `Plan` and `Task`, which the
disjointness axiom reports. This works only for nodes already typed by kind.
An untyped node takes its type from domain or range inference and is accepted,
so check 2 has to type every node before OWL is asked.

### What OWL cannot express

- **The window rules.** G1, S1, lower bounds, delivery in window, causality, and
  containment in an actor's span compare numbers and instants across individuals.
  OWL 2 data ranges constrain one literal against a constant. SWRL builtins can
  compare two literals in a rule, so S1 could be written as a rule that derives a
  violation. SWRL is outside OWL 2 DL, and no reasoner has been run on it here.
- **Acyclicity.** OWL's only cycle-like characteristic is irreflexivity, which
  forbids self-loops. `a feeds b` with `b feeds a` is consistent OWL.
- **Rootedness, and exact counts, as requirements.** OWL is open world. A node
  with no path to the root, or a parameter fed twice, is not a contradiction: the
  reasoner infers the missing link. A functional property does not refuse a second
  value; it infers the two are the same individual (`owl:sameAs`). A second
  `realizes` on one task would silently merge two plans. These are closed-world
  checks.
- **Unique node names.** OWL has no unique-name assumption. Two node IRIs can be
  one individual unless `owl:AllDifferent` says otherwise.
- **No duplicate edges.** Triples are sets, so a duplicate collapses. The reified
  form can hold one, and the graph pass refuses it.
- **Time values.** A `wait` window is a duration from the origin, and
  `time::Existence` is an interval. OWL has neither.
- **Window subtyping.** `future<Route>[wait ..5000]` refines `..5500` by inclusion
  of ranges, which OWL has no construct for.
- **Plans as signatures.** OWL has no functions, so parameter names and order are
  not represented. The parameters are the labelled `feeds` edges.
- **Sequencing, races and replanning.** These are operations, not logic.

### What the mapping needs first

- **Base IRIs** (MAPS §7.2). Without them a node name is not an IRI.
- **Node names as IRI fragments.** `nodes` is `[str]`, so `"fetch forecast"` is a
  legal node. The `owl::Individual` pattern refuses the bare name `route` as an
  IRI, which is what a node name is today (checked in the scratch project).

### Two findings

- **Direction lives on the property.** `directed` is a flag on each graph, but
  OWL's symmetry belongs to the relation, for the whole ontology. A `contains`
  relation cannot be symmetric in one graph and asymmetric in another, so
  undirected graphs need relation names that directed graphs do not use. Q12.
- **The `Task` name.** A class's IRI is under its universe's base. If the graph
  names a `plan:Task` class, it and the `Task` in `task.json` are different
  classes at the OWL level, while the authoring names still clash. Whether they
  are the same is Q6. `task.json` is not edited here.

## Open questions

- **Q1. Sequenced plans.** A plan called when an input arrives, rather than at the
  origin, has windows that add. That needs its own relationship. Not in v0.
- **Q2. Sub-graphs.** Can a plan be realized by a graph of tasks rather than one
  task? A nested graph would have its own origin, and the checks would run on each
  level.
- **Q3. Refinement in graphs.** A feed could accept a plan that refines the one it
  names, using the existing `refines`, which already requires a narrower window.
  Not designed here.
- **Q4. Races.** A plan that returns on the first of several inputs needs a
  different S1: its commitment must cover the smallest input budget, since the
  first input to arrive is due by that budget. Not in v0.
- **Q5. Where graphs live.** As a graph file (what this doc uses), which keeps the
  protocol frozen, or as a new top-level section of a universe, like `functions`,
  which is a protocol change.
- **Q6. Name clash.** `plan::PlanGraph` and the parent's `task.json` state machine
  may both want the word `Task`. The state machine could attach to a task's
  existence, or one of the names could change.
- **Q7. Absolute deadlines.** Windows are relative to the origin, in milliseconds.
  Should a plan be able to promise a `time::Moment` instead? That is a second kind
  of window.
- **Q8. Plans with no task.** Is an external commitment, a plan whose window
  someone else keeps, allowed? v0 allows it, provided the plan still reaches the
  root.
- **Q9. Defaulting lower bounds.** Consumers state no minimum by default, so early
  input is accepted. Is that the right default, or should a consumer state one?
- **Q10. Acyclic as a flag.** Acyclicity is asked for by the plan layer. Should it
  be a flag on `graph::Graph`, next to `directed` and `root`, so that other layers
  can ask for it too?
- **Q11. Undirected rootedness.** For an undirected graph, a root connects every
  node. Is "connected to the root" the right meaning, or should an undirected graph
  name a root only when it is a tree?
- **Q12. Where direction lives.** OWL puts symmetry on the relation, not the graph.
  Either undirected graphs use relation names that directed graphs never use, or
  the `directed` flag becomes a check that each graph's relations match their
  declared symmetry. Which one?
- **Q13. Node names.** Identifiers only, the same rule as unitype names, so IRI
  fragments need no escaping? Or any string, percent-encoded on export?

## Findings from building the example

- **Cross-universe `extends` is refused.** `job::Job.extends: time::Occurrence is
  in another universe; inheritance stays within a universe, so link the two types
  instead`. So a task outside `time` holds an existence as a field, as `actor.json`
  does. `time.json` says tasks "extend this", which is true only inside `time`. Not
  edited here.
- **Windows nest by subtyping, as the protocol says.** A narrower window refines a
  wider one. A wider one is refused, with the window in the message. A plain
  `future<i64>` refines no windowed one. A `[wait 0..5000]` does not refine a
  `[wait 100..5000]`, which checks the lower bound too.
- **Types cannot say everything.** `graph::Graph` accepts `root_reach` on an
  undirected graph, because the type has no way to tie a field to another field.
  That is why rule 3 in the graph section is a pass, not a type.

## Verification

- `unitype --project examples/time check`: `time checks out.` with 3 universes, 11
  types, 0 signatures. This is the same as before the change, because no file under
  `universes/` changed.
- In a scratch copy of the project, with every JSON block in this file written out
  as its own file: `check` passes, and `validate` accepts the graph values
  (`graph::Graph` for the two generic examples, `plan::PlanGraph` for
  `field_survey`). `validate` also accepts a `root_reach` on an undirected graph,
  which the graph pass will refuse (rule 3); the type cannot say it.
- Probes for the findings above, each in its own scratch project.
- OWL mapping: the `owl` universe from `examples/time/MAPS.md` (on `main`, 8d83bd0)
  was extracted into a scratch project with `time`, `actor`, `graph`, `plan` and
  `survey`. `check` passes (7 universes, 28 types, 5 signatures). `validate --type
  owl::Individual` accepts the reified `feeds` edge, and refuses it with a bare
  node name in place of an IRI. No OWL reasoner was run.

Nothing here checks feeds, cycles or windows in the checker yet. Those passes are
the next step, and they are what this design specifies.
