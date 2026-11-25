// `monad_test::explain` in Node.
//
// The same test as proofs/monad_test.py, written out again for Node: each
// instantiation is a constructor with its own `pure` and `bind` and the samples
// the laws are tried on. A verdict says whether the constructor passed, and if
// not, the first part that failed. Conform holds this file and the Python one
// to the same verdicts.
//
// The harness this imports is generated: run `unitype gen` in the project
// first, and it appears under generated/typescript. Node 24 or later runs this
// file as it is, by dropping the types.

import { unitypeMain } from "../generated/typescript/unitype_harness.ts";

type Step = (x: number) => unknown;

type Instance = {
  pure?: (a: number) => unknown;
  bind?: (m: unknown, step: Step) => unknown;
  values: number[];
  monadic: unknown[];
  f: Step;
  g: Step;
};

type Verdict = { held: boolean; reason: string | null };

const same = (a: unknown, b: unknown): boolean => JSON.stringify(a) === JSON.stringify(b);

const listBind = (m: unknown, step: Step): unknown =>
  (m as number[]).flatMap((x) => step(x) as number[]);

const optionalBind = (m: unknown, step: Step): unknown =>
  m === null ? null : step(m as number);

// Keeps only the first answer each step gives back: not a monad, since it loses
// what the step said.
const firstListBind = (m: unknown, step: Step): unknown => {
  const out: unknown[] = [];
  for (const x of m as number[]) {
    const answer = step(x) as unknown[];
    if (answer.length > 0) out.push(answer[0]);
  }
  return out;
};

const instances: Record<string, Instance> = {
  list: {
    pure: (a) => [a],
    bind: listBind,
    values: [3, 0, 7],
    monadic: [[1, 2], [], [4]],
    f: (x) => [x, x * 2],
    g: (y) => [y + 1],
  },
  optional: {
    pure: (a) => a,
    bind: optionalBind,
    values: [3, 0, 7],
    monadic: [5, null],
    f: (x) => (x < 10 ? x + 1 : null),
    g: (y) => (y < 100 ? y * 2 : null),
  },
  first_list: {
    pure: (a) => [a],
    bind: firstListBind,
    values: [3, 0, 7],
    monadic: [[1, 2], [], [4]],
    f: (x) => [x, x * 2],
    g: (y) => [y + 1],
  },
};

// The first law that breaks on the samples, or null if none does.
function firstLawFailed(inst: Instance & { pure: (a: number) => unknown; bind: (m: unknown, step: Step) => unknown }): string | null {
  const { pure, bind, f, g } = inst;
  for (const a of inst.values) {
    // Left identity: lifting a value and then stepping is the step itself.
    if (!same(bind(pure(a), f), f(a))) return "left_identity";
  }
  for (const m of inst.monadic) {
    // Right identity: stepping into `pure` changes nothing.
    if (!same(bind(m, pure), m)) return "right_identity";
    // Associativity: grouping the two steps either way gives one answer.
    if (!same(bind(bind(m, f), g), bind(m, (x) => bind(f(x), g)))) return "associativity";
  }
  return null;
}

function explain(name: string): Verdict {
  const inst = instances[name];
  if (inst === undefined) throw new Error(`no instantiation of this test for ${name}`);
  if (inst.pure === undefined) return { held: false, reason: "no_pure" };
  if (inst.bind === undefined) return { held: false, reason: "no_bind" };
  const law = firstLawFailed({ ...inst, pure: inst.pure, bind: inst.bind });
  return { held: law === null, reason: law };
}

unitypeMain({ "monad_test.explain": explain });
