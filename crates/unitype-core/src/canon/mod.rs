//! Canonical form and content addressing.
//!
//! One semantic type, one serialization, one address. [`normalize`] reduces a
//! type to its single representative form, [`encode`] pins down the bytes, and
//! [`hash`] turns those bytes into an identity that two systems can compare
//! without ever having exchanged a schema. [`words`] makes that identity
//! sayable.

pub mod encode;
pub mod hash;
pub mod normalize;
pub mod words;

pub use encode::{canonical_bytes, canonical_string, pretty_string};
pub use hash::{TYPE_ID_PREFIX, TypeId, TypeIdParseError, decl_hash, local_hash, sig_hash};
pub use normalize::{normalize_type, normalize_universe, normalize_workspace};
pub use words::{
    ALIAS_WORDS, AliasIndex, AliasLookup, MIN_ALIAS_WORDS, WORDLIST_VERSION, alias_words,
    full_alias,
};
