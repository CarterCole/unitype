//! What the checker reports, and where.
//!
//! Errors carry a path rather than a source span. The canonical form is a
//! structure, not a text layout, and a path into that structure —
//! `demo::Employee.role` — locates a problem in a way that survives the
//! document being reformatted, regenerated, or assembled from several files.

use std::fmt;

use crate::ident::{Ident, QualifiedName};
use crate::ir::Type;

/// One step along the path to a problem.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathSeg {
    /// Inside a universe.
    Universe(Ident),
    /// Inside a type declaration.
    Def(Ident),
    /// Inside a function signature.
    Sig(Ident),
    /// Inside a link.
    Link(Ident),
    /// At a named field.
    Field(Ident),
    /// At a variant of an enum.
    Variant(Ident),
    /// At a positional parameter.
    Param(usize),
    /// At a generic argument.
    Arg(usize),
    /// At a union member or tuple element.
    Member(usize),
    /// At the `extends` clause.
    Extends,
    /// At one entry of an `implements` clause.
    Implements(usize),
    /// At a generic parameter's bound.
    Bound(usize),
    /// At the return type.
    Ret,
    /// At the element type of a list, optional, or future.
    Elem,
    /// At the key type of a map.
    Key,
    /// At a signature's body.
    Body,
}

impl fmt::Display for PathSeg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PathSeg::Universe(n) => write!(f, "{n}"),
            PathSeg::Def(n) | PathSeg::Sig(n) => write!(f, "::{n}"),
            PathSeg::Link(n) => write!(f, "link {n}"),
            PathSeg::Field(n) => write!(f, ".{n}"),
            PathSeg::Variant(n) => write!(f, "|{n}"),
            PathSeg::Param(i) => write!(f, ".params[{i}]"),
            PathSeg::Arg(i) => write!(f, "<{i}>"),
            PathSeg::Member(i) => write!(f, "[{i}]"),
            PathSeg::Extends => f.write_str(".extends"),
            PathSeg::Implements(i) => write!(f, ".implements[{i}]"),
            PathSeg::Bound(i) => write!(f, ".bounds[{i}]"),
            PathSeg::Ret => f.write_str(".ret"),
            PathSeg::Elem => f.write_str("[]"),
            PathSeg::Key => f.write_str(".key"),
            PathSeg::Body => f.write_str(".body"),
        }
    }
}

/// Where in a workspace a problem is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TypePath(pub Vec<PathSeg>);

impl TypePath {
    /// The empty path, meaning the workspace itself.
    pub fn root() -> Self {
        Self::default()
    }

    /// This path extended by one step.
    pub fn then(&self, seg: PathSeg) -> Self {
        let mut segs = self.0.clone();
        segs.push(seg);
        Self(segs)
    }
}

impl fmt::Display for TypePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            return f.write_str("<workspace>");
        }
        for seg in &self.0 {
            write!(f, "{seg}")?;
        }
        Ok(())
    }
}

/// What went wrong.
#[derive(thiserror::Error, Debug, Clone, PartialEq)]
pub enum CheckErrorKind {
    /// A reference named a universe that is not loaded.
    #[error("unknown universe `{0}`")]
    UnknownUniverse(Ident),
    /// A reference named a type that does not exist.
    #[error("unknown type `{0}`")]
    UnknownType(QualifiedName),
    /// A reference named a signature that does not exist.
    #[error("unknown signature `{0}`")]
    UnknownFunction(QualifiedName),
    /// Two universes share a name.
    #[error("universe `{0}` is declared more than once")]
    DuplicateUniverse(Ident),
    /// Two declarations in one universe share a name.
    #[error("`{0}` is declared more than once in this universe")]
    DuplicateDef(Ident),
    /// Two signatures in one universe share a name.
    #[error("signature `{0}` is declared more than once in this universe")]
    DuplicateFunction(Ident),
    /// A record or struct declared the same field twice.
    #[error("field `{0}` is declared more than once")]
    DuplicateField(Ident),
    /// A generic reference had the wrong number of arguments.
    #[error("`{name}` takes {expected} type argument(s), but {got} were given")]
    ArityMismatch {
        /// The declaration being referenced.
        name: QualifiedName,
        /// How many parameters it declares.
        expected: usize,
        /// How many arguments were supplied.
        got: usize,
    },
    /// A generic parameter was used where nothing bound it.
    #[error("`{0}` is not a generic parameter of this declaration")]
    UnboundParam(Ident),
    /// A map key type could not be represented as an object key.
    #[error("`{0}` cannot be a map key: use a string, an integer, or a literal")]
    BadMapKey(String),
    /// Aliases expanded into each other forever.
    #[error("alias cycle: {}", .0.join(" -> "))]
    AliasCycle(Vec<String>),
    /// A struct inherited from itself, directly or by a longer path.
    #[error("inheritance cycle: {}", .0.join(" -> "))]
    ExtendsCycle(Vec<String>),
    /// A struct extended something that is not a struct.
    #[error("`{name}` cannot be extended: it is {article} {kind}, not a struct")]
    ParentNotStruct {
        /// What was named as the parent.
        name: QualifiedName,
        /// The article suiting `kind`.
        article: &'static str,
        /// What the target actually is.
        kind: &'static str,
    },
    /// A struct extended a struct in another universe.
    ///
    /// Inheritance is a claim about one world's structure. Relating types
    /// across universes is what links are for.
    #[error(
        "`{parent}` is in another universe; inheritance stays within a universe, so link the two types instead"
    )]
    ParentInOtherUniverse {
        /// The parent that was named.
        parent: QualifiedName,
    },
    /// Something that is not an interface appeared where one was required.
    #[error("`{name}` is {article} {kind}, not an interface")]
    NotAnInterface {
        /// What was named.
        name: QualifiedName,
        /// The article suiting `kind`.
        article: &'static str,
        /// What the target actually is.
        kind: &'static str,
    },
    /// A field override widened rather than narrowed its type.
    #[error(
        "field `{field}` overrides `{parent}`, but `{found}` is not a subtype of the inherited `{required}`"
    )]
    BadOverride {
        /// The field being overridden.
        field: Ident,
        /// Where the inherited declaration came from.
        parent: QualifiedName,
        /// The inherited type.
        required: String,
        /// The overriding type.
        found: String,
    },
    /// Two interfaces demanded the same field with different types.
    #[error("interfaces `{a}` and `{b}` both require field `{field}`, with incompatible types")]
    InterfaceFieldConflict {
        /// The contested field.
        field: Ident,
        /// One interface.
        a: QualifiedName,
        /// The other.
        b: QualifiedName,
    },
    /// A struct claimed an interface it does not satisfy.
    #[error("`{interface}` requires field `{field}`, which is missing")]
    MissingInterfaceField {
        /// The interface claimed.
        interface: QualifiedName,
        /// The field it requires.
        field: Ident,
    },
    /// A struct had an interface's field but with an unusable type.
    #[error("`{interface}` requires `{field}: {required}`, but this declares `{found}`")]
    InterfaceFieldMismatch {
        /// The interface claimed.
        interface: QualifiedName,
        /// The field in question.
        field: Ident,
        /// The type the interface requires.
        required: String,
        /// The type actually declared.
        found: String,
    },
    /// A generic argument did not satisfy its parameter's bound.
    #[error("`{arg}` does not satisfy the bound `{bound}`")]
    BoundNotSatisfied {
        /// The argument supplied.
        arg: String,
        /// The bound it had to meet.
        bound: QualifiedName,
    },
    /// A signature claimed to refine one that does not exist.
    #[error("`{0}` is not a signature, so nothing can refine it")]
    UnknownRefinement(QualifiedName),
    /// A refinement was not usable in place of what it refines.
    ///
    /// A proof of the refinement has to be a proof of the original, so the
    /// refinement's type must be usable wherever the original's is.
    #[error(
        "`{sig}` cannot refine `{base}`: `{sig_ty}` is not usable where `{base_ty}` is expected"
    )]
    RefinementNotSubtype {
        /// The refining signature.
        sig: QualifiedName,
        /// What it claims to refine.
        base: QualifiedName,
        /// The refinement's function type.
        sig_ty: String,
        /// The refined signature's function type.
        base_ty: String,
    },
    /// Refinement claims formed a loop.
    #[error("refinement cycle: {}", .0.join(" -> "))]
    RefinementCycle(Vec<String>),
    /// A body used a name nothing bound.
    #[error("`{0}` has nothing bound to it here")]
    UnboundVar(Ident),
    /// A body read a field off something without one.
    #[error("`{ty}` has no field `{field}`")]
    NoSuchField {
        /// The type being read.
        ty: String,
        /// The field that was asked for.
        field: Ident,
    },
    /// A body tried to construct something that cannot be constructed that way.
    #[error("`{name}` is not {what}, so it cannot be built this way")]
    NotConstructible {
        /// What was named.
        name: QualifiedName,
        /// What building this way requires.
        what: &'static str,
    },
    /// A constructed value left a required field unset.
    #[error("`{name}` requires field `{field}`, which is not set here")]
    MissingFieldInit {
        /// What was being built.
        name: QualifiedName,
        /// The field left unset.
        field: Ident,
    },
    /// A field was set to something of the wrong type.
    #[error("field `{field}`: `{found}` cannot be used where `{required}` is expected")]
    FieldInitMismatch {
        /// The field being set.
        field: Ident,
        /// The type supplied.
        found: String,
        /// The type required.
        required: String,
    },
    /// A body named a variant an enum does not have.
    #[error("`{name}` has no variant `{tag}`")]
    NoSuchVariant {
        /// The enum.
        name: QualifiedName,
        /// The variant that was named.
        tag: Ident,
    },
    /// A call inside a body did not type-check.
    #[error("{0}")]
    BadCall(String),
    /// A conditional was given something other than a boolean to branch on.
    #[error("a condition must be a boolean, but this is `{found}`")]
    ConditionNotBool {
        /// What was found instead.
        found: String,
    },
    /// A body mapped through a constructor the value was not built with.
    #[error("`{found}` is not {wanted}, so there is nothing to map through")]
    NotMappable {
        /// What was found instead.
        found: String,
        /// The constructor the body expected.
        wanted: &'static str,
    },
    /// A body did not produce what its signature promised.
    #[error("this body does not produce `{required}`; it produces `{found}`")]
    BodyTypeMismatch {
        /// What the body produces.
        found: String,
        /// What the signature promised.
        required: String,
    },
    /// Bodies called each other in a loop.
    #[error("these bodies call each other and so would never finish: {}", .0.join(" -> "))]
    BodyCycle(Vec<String>),
    /// Two links share a name.
    #[error("link `{0}` is declared more than once")]
    DuplicateLink(Ident),
    /// A conversion referred to a link that is not declared.
    #[error("link `{0}` is not declared")]
    UnknownLink(Ident),
    /// A link joined two types in the same universe.
    #[error("`{from}` and `{to}` are in the same universe; a link crosses between universes")]
    LinkWithinUniverse {
        /// The source.
        from: QualifiedName,
        /// The target.
        to: QualifiedName,
    },
    /// A link claimed two types are the same when they are not.
    #[error("`{from}` and `{to}` do not hold the same data, so this is not a plain cast")]
    NotIdentical {
        /// The source.
        from: QualifiedName,
        /// The target.
        to: QualifiedName,
    },
    /// A field mapping was declared between things that have no fields.
    #[error("`{name}` has no fields, so there is nothing to map")]
    LinkEndpointHasNoFields {
        /// The endpoint at fault.
        name: QualifiedName,
    },
    /// A mapping named a field that is not there.
    #[error("no such field `{field}` on either end of this link")]
    LinkNamesNoSuchField {
        /// The name that matched nothing.
        field: Ident,
    },
    /// A mapping left a required field with no source.
    #[error("this link has nothing to fill `{to}`'s `{field}`, which cannot be left out")]
    LinkLeavesFieldUnfilled {
        /// The field with no source.
        field: Ident,
        /// The type that requires it.
        to: QualifiedName,
    },
    /// A witness did not have the type the link needs.
    #[error("`{sig}` does not convert a `{from}` into a `{to}`; it is `{found}`")]
    WitnessDoesNotConvert {
        /// The signature named as the witness.
        sig: QualifiedName,
        /// The source.
        from: QualifiedName,
        /// The target.
        to: QualifiedName,
        /// The type it actually has.
        found: String,
    },
    /// A bridge discards fields. Legitimate, but worth saying out loud.
    #[error("converting `{from}` to `{to}` discards {}", .fields.join(", "))]
    LossyLink {
        /// The source.
        from: QualifiedName,
        /// The target.
        to: QualifiedName,
        /// The fields that do not survive.
        fields: Vec<String>,
    },
    /// A signature refined one with a different arity.
    #[error("`{sig}` takes {got} parameter(s) but refines `{base}`, which takes {expected}")]
    RefinementArity {
        /// The refining signature.
        sig: QualifiedName,
        /// What it refines.
        base: QualifiedName,
        /// The refined signature's parameter count.
        expected: usize,
        /// The refinement's parameter count.
        got: usize,
    },
}

impl CheckErrorKind {
    /// The article that reads correctly before this kind's noun.
    pub(crate) fn article_for(kind: &str) -> &'static str {
        match kind.chars().next() {
            Some('a' | 'e' | 'i' | 'o' | 'u') => "an",
            _ => "a",
        }
    }
}

/// A problem, and where it is.
#[derive(Clone, Debug, PartialEq)]
pub struct CheckError {
    /// What went wrong.
    pub kind: CheckErrorKind,
    /// Where in the workspace.
    pub path: TypePath,
}

impl CheckError {
    /// Reports `kind` at `path`.
    pub fn new(kind: CheckErrorKind, path: TypePath) -> Self {
        Self { kind, path }
    }
}

impl fmt::Display for CheckError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path, self.kind)
    }
}

/// Everything the checker found.
///
/// The checker reports as much as it can rather than stopping at the first
/// problem, because fixing one at a time is a poor way to spend an afternoon.
#[derive(thiserror::Error, Debug, Clone, Default, PartialEq)]
pub struct Diagnostics {
    /// Problems that make the workspace unusable.
    pub errors: Vec<CheckError>,
    /// Problems worth knowing about that do not stop anything.
    pub warnings: Vec<CheckError>,
}

impl Diagnostics {
    /// True when nothing was found that stops the workspace being used.
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }

    /// Records a problem.
    pub fn error(&mut self, kind: CheckErrorKind, path: TypePath) {
        self.errors.push(CheckError::new(kind, path));
    }

    /// Records something worth knowing about.
    pub fn warn(&mut self, kind: CheckErrorKind, path: TypePath) {
        self.warnings.push(CheckError::new(kind, path));
    }

    /// Folds another set of findings into this one.
    pub fn extend(&mut self, other: Diagnostics) {
        self.errors.extend(other.errors);
        self.warnings.extend(other.warnings);
    }
}

impl fmt::Display for Diagnostics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let total = self.errors.len();
        writeln!(f, "{total} problem{}:", if total == 1 { "" } else { "s" })?;
        for e in &self.errors {
            writeln!(f, "  {e}")?;
        }
        for w in &self.warnings {
            writeln!(f, "  warning: {w}")?;
        }
        Ok(())
    }
}

/// Describes a type the way a person would write it.
///
/// Canonical JSON is what gets hashed; this is what gets read. An error saying
/// `[str]?` is one somebody can act on, where the same thing spelled as nested
/// objects is one they have to decode first.
pub(crate) fn describe(t: &Type) -> String {
    t.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> Ident {
        Ident::new(s).unwrap()
    }

    #[test]
    fn a_path_reads_like_the_place_it_points_to() {
        let path = TypePath::root()
            .then(PathSeg::Universe(id("demo")))
            .then(PathSeg::Def(id("Employee")))
            .then(PathSeg::Field(id("role")));
        assert_eq!(path.to_string(), "demo::Employee.role");
    }

    #[test]
    fn a_path_into_a_nested_type_shows_the_way_in() {
        let path = TypePath::root()
            .then(PathSeg::Universe(id("demo")))
            .then(PathSeg::Sig(id("load")))
            .then(PathSeg::Ret)
            .then(PathSeg::Elem);
        assert_eq!(path.to_string(), "demo::load.ret[]");
    }

    #[test]
    fn the_empty_path_names_the_workspace() {
        assert_eq!(TypePath::root().to_string(), "<workspace>");
    }

    #[test]
    fn findings_accumulate_rather_than_stopping_at_the_first() {
        let mut d = Diagnostics::default();
        assert!(d.is_ok());
        d.error(CheckErrorKind::UnknownUniverse(id("a")), TypePath::root());
        d.error(CheckErrorKind::UnknownUniverse(id("b")), TypePath::root());
        assert!(!d.is_ok());
        assert_eq!(d.errors.len(), 2);
    }

    #[test]
    fn articles_suit_the_word_that_follows() {
        assert_eq!(CheckErrorKind::article_for("interface"), "an");
        assert_eq!(CheckErrorKind::article_for("enum"), "an");
        assert_eq!(CheckErrorKind::article_for("struct"), "a");
    }
}
