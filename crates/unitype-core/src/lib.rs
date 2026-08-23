//! The symbolic core of unitype: a language-neutral representation of types,
//! the values that inhabit them, and the propositions that relate them.
//!
//! # The idea
//!
//! A function signature is a *proposition*; an implementation of it is a
//! *proof*. There can be many proofs of one proposition — in different
//! languages, with different costs — and choosing among them at call time is
//! what makes data and computation portable between systems. This crate holds
//! the half of that idea which is pure: the representation, its canonical form,
//! its identity, and the rules that decide what is well-typed. Running proofs
//! lives in `unitype-runtime`; writing them out in a target language lives in
//! `unitype-codegen`.
//!
//! # Canonical form and identity
//!
//! Every type has exactly one canonical serialization, and a type's identity is
//! the hash of those bytes. Two systems that have never met agree on whether
//! they hold the same type by comparing 32 bytes, with no shared registry and
//! no coordination. Names, documentation, and annotations are excluded from
//! that hash, because they are not what makes a type what it is.
//!
//! # Everything is a literal
//!
//! Types, declarations, values, and expressions all serialize to plain JSON, so
//! the representation is also the wire format and the authoring format. A type
//! read out of one system can be written into another without a schema
//! compiler in between.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod canon;
pub mod ident;
pub mod ir;
pub mod link;
pub mod meta;
pub mod value;

pub use canon::{TypeId, canonical_string, decl_hash, full_alias, local_hash, sig_hash};
pub use ident::{Ident, IdentError, QualifiedName};
pub use ir::{
    Expr, Field, FunctionSig, LiteralValue, LoadError, SCHEMA_VERSION, Type, TypeDef, TypeRef,
    Universe, Workspace,
};
pub use link::{Link, LinkKind};
pub use meta::{BigO, ImplProfile, Metadata};
pub use value::Value;
