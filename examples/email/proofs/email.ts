// `email::contacted` and `email::verified` in Node, over the same simulated
// mailboxes as proofs/email.py. Nothing here sends mail. Conform holds the two
// files to the same answers.
//
// The harness this imports is generated: run `unitype gen` in the project
// first, and it appears under generated/typescript. Node 24 or later runs this
// file as it is, by dropping the types.

import { unitypeMain } from "../generated/typescript/unitype_harness.ts";

type Receiver = { address: string; provider: "inbox" | "bounce" };

const mailboxes: Record<string, { answersFor: Set<string> }> = {
  inbox: { answersFor: new Set(["ada@example.com"]) },
  bounce: { answersFor: new Set() },
};

// Mail sent to the address is delivered: an inbox takes any address with an at
// sign in it, and a bounce takes none.
function contacted(receiver: Receiver): boolean {
  return receiver.provider === "inbox" && receiver.address.includes("@");
}

// The mailbox answers for the address only when it owns it.
function verified(receiver: Receiver): boolean {
  return mailboxes[receiver.provider].answersFor.has(receiver.address);
}

unitypeMain({ "email.contacted": contacted, "email.verified": verified });
