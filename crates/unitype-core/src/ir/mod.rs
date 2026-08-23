//! The symbolic representation: types, declarations, expressions, universes,
//! and the workspaces that hold them.

pub mod defs;
pub mod expr;
pub mod sugar;
pub mod ty;
pub mod universe;
pub mod workspace;

pub use defs::{
    AliasDef, EnumDef, FunctionSig, InterfaceDef, NamedParam, StructDef, TypeDef, TypeParamDecl,
    Variant,
};
pub use expr::{Expr, FieldInit};
pub use sugar::SugarError;
pub use ty::{Field, LiteralValue, Type, TypeRef};
pub use universe::Universe;
pub use workspace::{LoadError, SCHEMA_VERSION, Workspace};
