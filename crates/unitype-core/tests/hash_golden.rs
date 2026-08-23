//! Pinned content addresses and word aliases.
//!
//! These vectors exist to make canonical-form drift impossible to do by
//! accident. Any change to normalization, to the byte encoding, or to a hash
//! domain will break this test — which is the point. A deliberate change means
//! bumping the domain version string and regenerating, and understanding that
//! every previously published address has changed.
//!
//! Regenerate with `UNITYPE_UPDATE_GOLDEN=1 cargo test -p unitype-core`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use unitype_core::canon::{decl_hash, full_alias, local_hash, sig_hash};
use unitype_core::ir::{
    AliasDef, Field, FunctionSig, NamedParam, StructDef, Type, TypeDef, TypeParamDecl, TypeRef,
};
use unitype_core::{Ident, Metadata};

fn id(s: &str) -> Ident {
    Ident::new(s).unwrap()
}

/// Every vector, as `name -> "<address> <alias>"`.
fn vectors() -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut add = |name: &str, address: unitype_core::TypeId| {
        out.insert(
            name.to_string(),
            format!("{address} {}", full_alias(&address)),
        );
    };

    // Primitives and the lattice extremes.
    for (name, ty) in [
        ("type/never", Type::Never),
        ("type/any", Type::Any),
        ("type/unit", Type::Unit),
        ("type/bool", Type::Bool),
        ("type/i32", Type::I32),
        ("type/i64", Type::I64),
        ("type/f64", Type::F64),
        ("type/string", Type::String),
        ("type/bytes", Type::Bytes),
    ] {
        add(name, local_hash(&ty));
    }

    // Constructors.
    add("type/list-i64", local_hash(&Type::I64.list()));
    add("type/optional-string", local_hash(&Type::String.optional()));
    add("type/future-bool", local_hash(&Type::Bool.future()));
    add(
        "type/map-string-i64",
        local_hash(&Type::I64.map_from(Type::String)),
    );
    add(
        "type/tuple-i32-string",
        local_hash(&Type::Tuple {
            items: vec![Type::I32, Type::String],
        }),
    );
    add(
        "type/union-i64-string",
        local_hash(&Type::Union {
            members: vec![Type::I64, Type::String],
        }),
    );
    add(
        "type/record-x-y",
        local_hash(&Type::Record {
            fields: vec![
                Field::new(id("x"), Type::F64),
                Field::new(id("y"), Type::F64),
            ],
        }),
    );
    add(
        "type/function-i64-to-bool",
        local_hash(&Type::Function {
            params: vec![Type::I64],
            ret: Box::new(Type::Bool),
        }),
    );
    add(
        "type/ref-qualified",
        local_hash(&Type::Ref(TypeRef::qualified(id("geo"), id("Point")))),
    );

    // A self-recursive declaration: the case a Merkle encoding has to get right.
    add(
        "decl/tree",
        decl_hash(&TypeDef::Struct(StructDef {
            name: id("Tree"),
            doc: None,
            params: vec![],
            extends: None,
            implements: vec![],
            fields: vec![
                Field::new(id("value"), Type::I64),
                Field::new(
                    id("children"),
                    Type::Ref(TypeRef::qualified(id("demo"), id("Tree"))).list(),
                ),
            ],
            meta: Metadata::default(),
        })),
    );

    // Inheritance.
    add(
        "decl/circle-extends-shape",
        decl_hash(&TypeDef::Struct(StructDef {
            name: id("Circle"),
            doc: None,
            params: vec![],
            extends: Some(TypeRef::qualified(id("demo"), id("Shape"))),
            implements: vec![],
            fields: vec![Field::new(id("radius"), Type::F64)],
            meta: Metadata::default(),
        })),
    );

    add(
        "decl/alias-id",
        decl_hash(&TypeDef::Alias(AliasDef {
            name: id("Id"),
            doc: None,
            params: vec![],
            target: Type::String,
            meta: Metadata::default(),
        })),
    );

    // Propositions.
    add(
        "sig/sort",
        sig_hash(&FunctionSig {
            name: id("sort"),
            doc: None,
            params_decl: vec![],
            params: vec![NamedParam::new(id("xs"), Type::I64.list())],
            ret: Type::I64.list(),
            refines: None,
            body: None,
            meta: Metadata::default(),
        }),
    );
    add(
        "sig/generic-identity",
        sig_hash(&FunctionSig {
            name: id("identity"),
            doc: None,
            params_decl: vec![TypeParamDecl::new(id("T"))],
            params: vec![NamedParam::new(id("v"), Type::Param { name: id("T") })],
            ret: Type::Param { name: id("T") },
            refines: None,
            body: None,
            meta: Metadata::default(),
        }),
    );
    add(
        "sig/async-lookup",
        sig_hash(&FunctionSig {
            name: id("find_user"),
            doc: None,
            params_decl: vec![],
            params: vec![NamedParam::new(id("id"), Type::I64)],
            ret: Type::Ref(TypeRef::qualified(id("app"), id("User")))
                .optional()
                .future(),
            refines: None,
            body: None,
            meta: Metadata::default(),
        }),
    );

    out
}

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/hashes.json")
}

#[test]
fn addresses_and_aliases_match_the_pinned_vectors() {
    let actual = vectors();
    let path = golden_path();

    let updating = std::env::var("UNITYPE_UPDATE_GOLDEN").is_ok();
    if updating || !path.exists() {
        std::fs::create_dir_all(path.parent().expect("golden dir")).expect("create golden dir");
        let text = serde_json::to_string_pretty(&actual).expect("serialize vectors");
        std::fs::write(&path, format!("{text}\n")).expect("write vectors");
        if !updating {
            panic!(
                "wrote initial golden vectors to {}; re-run to verify them",
                path.display()
            );
        }
        return;
    }

    let text = std::fs::read_to_string(&path).expect("read vectors");
    let expected: BTreeMap<String, String> = serde_json::from_str(&text).expect("parse vectors");

    let mut drifted = Vec::new();
    for (name, got) in &actual {
        match expected.get(name) {
            Some(want) if want == got => {}
            Some(want) => drifted.push(format!("  {name}\n    was {want}\n    now {got}")),
            None => drifted.push(format!("  {name}\n    new  {got}")),
        }
    }
    for name in expected.keys() {
        if !actual.contains_key(name) {
            drifted.push(format!("  {name}\n    vector no longer produced"));
        }
    }

    assert!(
        drifted.is_empty(),
        "canonical form drifted, so every published address has changed:\n{}\n\n\
         If this was deliberate, bump the hash domain version and regenerate with \
         UNITYPE_UPDATE_GOLDEN=1.",
        drifted.join("\n")
    );
}

#[test]
fn aliases_are_three_ordinary_words() {
    for entry in vectors().values() {
        let alias = entry.split_whitespace().nth(1).expect("alias present");
        let words: Vec<&str> = alias.split('-').collect();
        assert_eq!(words.len(), 11, "the full alias is eleven words: {alias}");
        for word in words {
            assert!(
                word.chars().all(|c| c.is_ascii_lowercase()),
                "`{word}` should be a plain lowercase word"
            );
        }
    }
}
