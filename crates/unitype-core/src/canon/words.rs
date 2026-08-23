//! Word aliases: a name for a type that a person can say out loud.
//!
//! A content address is exact and unmemorable. What3words solved the same
//! problem for locations by naming every square on Earth with three ordinary
//! words, and the sibling `interval_clock` project does it for periodic time.
//! This is the same convention applied to the space of types: derive words from
//! the address, and let people trade the words.
//!
//! Because the words come from the address, and the address comes from the
//! structure, two people who have never spoken derive the *same* words for the
//! same type — no registry, no coordination. "Do we have the same type?"
//! becomes a question you can settle in conversation.
//!
//! # Derivation
//!
//! The frozen BIP-39 English wordlist, 2048 words at 11 bits each. The address
//! is hashed once more in its own domain, and the top 121 bits become eleven
//! words. The shareable alias is the shortest prefix of those words that is
//! unambiguous among the types in hand, never fewer than three.
//!
//! Prefixes behave like abbreviated git hashes: showing more words is always
//! correct, and a collision lengthens the newcomer's alias rather than changing
//! anyone else's. **An alias is not an identity.** It is a handle for people;
//! `ut1:` addresses remain the authority.

use std::sync::LazyLock;

use crate::canon::hash::TypeId;

/// Domain separating alias derivation from every other hash in the system.
const DOMAIN_ALIAS: &str = "unitype alias v1";

/// The wordlist this build derives aliases from.
pub const WORDLIST_VERSION: &str = "bip39-english-v1";

/// Bits consumed per word: 2048 words is 2^11.
const BITS_PER_WORD: usize = 11;

/// How many words the full alias has.
pub const ALIAS_WORDS: usize = 11;

/// The fewest words an alias is ever shortened to.
///
/// Three words is roughly 33 bits, which is the familiar what3words trade: long
/// enough to be unambiguous in any workspace a person deals with, short enough
/// to say.
pub const MIN_ALIAS_WORDS: usize = 3;

static WORDS: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    let list: Vec<&'static str> = include_str!("wordlist_bip39_english_v1.txt")
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(
        list.len(),
        1 << BITS_PER_WORD,
        "the wordlist must hold exactly 2048 words"
    );
    list
});

/// The full eleven-word sequence for an address.
pub fn alias_words(id: &TypeId) -> Vec<&'static str> {
    let digest = blake3::Hasher::new_derive_key(DOMAIN_ALIAS)
        .update(id.as_bytes())
        .finalize();
    let bytes = digest.as_bytes();

    (0..ALIAS_WORDS)
        .map(|word| {
            let index = (0..BITS_PER_WORD).fold(0usize, |acc, bit| {
                let offset = word * BITS_PER_WORD + bit;
                let byte = bytes[offset / 8];
                let set = byte & (1 << (7 - offset % 8)) != 0;
                (acc << 1) | usize::from(set)
            });
            WORDS[index]
        })
        .collect()
}

/// The full alias for an address, hyphen-joined.
pub fn full_alias(id: &TypeId) -> String {
    alias_words(id).join("-")
}

/// What a lookup by alias prefix found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AliasLookup {
    /// Exactly one address has this prefix.
    Unique(TypeId),
    /// Several do, and the caller needs to say more words.
    Ambiguous(Vec<TypeId>),
    /// None do.
    NotFound,
}

/// The aliases in play for one set of addresses.
///
/// Shortening is relative to a set: an alias is as short as it can be while
/// still picking out one address *among those present*. Adding types can make a
/// new alias longer, but never changes an address's word sequence.
#[derive(Clone, Debug, Default)]
pub struct AliasIndex {
    entries: Vec<(TypeId, Vec<&'static str>)>,
}

impl AliasIndex {
    /// Builds an index over a set of addresses.
    pub fn new(ids: impl IntoIterator<Item = TypeId>) -> Self {
        let mut entries: Vec<(TypeId, Vec<&'static str>)> =
            ids.into_iter().map(|id| (id, alias_words(&id))).collect();
        entries.sort_by_key(|(id, _)| *id);
        entries.dedup_by(|a, b| a.0 == b.0);
        Self { entries }
    }

    /// How many addresses this index covers.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when the index covers nothing.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The shortest unambiguous alias for an address in this index.
    ///
    /// Returns `None` for an address the index does not cover. In the
    /// vanishingly unlikely event that two addresses agree on all eleven words,
    /// the full alias is returned rather than an error: the words were never
    /// the identity.
    pub fn alias_for(&self, id: &TypeId) -> Option<String> {
        let words = &self.entries.iter().find(|(other, _)| other == id)?.1;
        for length in MIN_ALIAS_WORDS..=ALIAS_WORDS {
            let unique = !self
                .entries
                .iter()
                .any(|(other, w)| other != id && w[..length] == words[..length]);
            if unique {
                return Some(words[..length].join("-"));
            }
        }
        Some(words.join("-"))
    }

    /// Resolves a hyphen-joined prefix of words back to an address.
    pub fn resolve(&self, prefix: &str) -> AliasLookup {
        let wanted: Vec<&str> = prefix
            .split('-')
            .map(str::trim)
            .filter(|w| !w.is_empty())
            .collect();
        if wanted.is_empty() {
            return AliasLookup::NotFound;
        }

        let matches: Vec<TypeId> = self
            .entries
            .iter()
            .filter(|(_, words)| words.len() >= wanted.len() && words[..wanted.len()] == wanted[..])
            .map(|(id, _)| *id)
            .collect();

        match matches.len() {
            0 => AliasLookup::NotFound,
            1 => AliasLookup::Unique(matches[0]),
            _ => AliasLookup::Ambiguous(matches),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canon::hash::local_hash;
    use crate::ir::Type;

    fn some_ids(n: usize) -> Vec<TypeId> {
        (0..n)
            .map(|i| {
                local_hash(&Type::Tuple {
                    items: vec![Type::I64; i + 2],
                })
            })
            .collect()
    }

    #[test]
    fn the_wordlist_is_the_frozen_bip39_english_list() {
        assert_eq!(WORDS.len(), 2048);
        assert_eq!(WORDS[0], "abandon");
        assert_eq!(WORDS[2047], "zoo");
    }

    #[test]
    fn an_address_always_derives_the_same_words() {
        let id = local_hash(&Type::String);
        assert_eq!(alias_words(&id), alias_words(&id));
        assert_eq!(alias_words(&id).len(), ALIAS_WORDS);
    }

    #[test]
    fn the_same_structure_gets_the_same_words_however_it_was_written() {
        let one = local_hash(&Type::Union {
            members: vec![Type::I64, Type::String],
        });
        let other = local_hash(&Type::Union {
            members: vec![Type::String, Type::Never, Type::I64],
        });
        assert_eq!(full_alias(&one), full_alias(&other));
    }

    #[test]
    fn different_structures_get_different_words() {
        assert_ne!(
            full_alias(&local_hash(&Type::I64)),
            full_alias(&local_hash(&Type::String))
        );
    }

    #[test]
    fn every_word_comes_from_the_list() {
        for id in some_ids(20) {
            for word in alias_words(&id) {
                assert!(WORDS.contains(&word), "`{word}` is not in the wordlist");
            }
        }
    }

    #[test]
    fn an_alias_is_three_words_when_three_words_will_do() {
        let index = AliasIndex::new(some_ids(50));
        for id in some_ids(50) {
            let alias = index.alias_for(&id).expect("covered");
            assert_eq!(
                alias.split('-').count(),
                MIN_ALIAS_WORDS,
                "no collision was expected among 50 types, got `{alias}`"
            );
        }
    }

    #[test]
    fn an_alias_resolves_back_to_the_address_it_names() {
        let ids = some_ids(30);
        let index = AliasIndex::new(ids.clone());
        for id in ids {
            let alias = index.alias_for(&id).expect("covered");
            assert_eq!(index.resolve(&alias), AliasLookup::Unique(id));
        }
    }

    #[test]
    fn saying_more_words_than_needed_still_works() {
        let ids = some_ids(5);
        let index = AliasIndex::new(ids.clone());
        let full = full_alias(&ids[0]);
        assert_eq!(index.resolve(&full), AliasLookup::Unique(ids[0]));
    }

    #[test]
    fn too_few_words_reports_the_candidates_rather_than_guessing() {
        let ids = some_ids(40);
        let index = AliasIndex::new(ids.clone());
        let first_word = alias_words(&ids[0])[0];
        // A single word is far too little to be unique across the whole list.
        let shared: Vec<_> = ids
            .iter()
            .filter(|i| alias_words(i)[0] == first_word)
            .collect();
        if shared.len() > 1 {
            assert!(matches!(
                index.resolve(first_word),
                AliasLookup::Ambiguous(_)
            ));
        }
        assert_eq!(index.resolve("notaword-at-all"), AliasLookup::NotFound);
        assert_eq!(index.resolve(""), AliasLookup::NotFound);
    }

    #[test]
    fn an_address_outside_the_index_has_no_alias_in_it() {
        let index = AliasIndex::new(some_ids(3));
        assert_eq!(index.alias_for(&local_hash(&Type::Bytes)), None);
    }
}
