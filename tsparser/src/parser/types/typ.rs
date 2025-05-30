use crate::parser::types::type_resolve::Ctx;
use crate::parser::types::{object, validation, Object, ResolveState};
use crate::parser::Range;
use indexmap::IndexMap;
use itertools::Itertools;
use serde::Serialize;
use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::{Debug, Display};
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use swc_common::errors::HANDLER;

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct TypeArgId(usize);

#[derive(Debug, Clone, Hash, Serialize)]
pub enum Type {
    /// strings, etc
    Basic(Basic),
    /// T[], Array<T>
    Array(Array),
    /// { foo: string }
    Interface(Interface),
    /// a | b | c
    Union(Union),
    /// [string, number]
    Tuple(Tuple),
    /// "foo"
    Literal(Literal),
    /// class Foo {}
    Class(ClassType),
    /// enum Foo {}
    Enum(EnumType),

    /// A named type, with optional type arguments.
    Named(Named),

    /// e.g. "string?" in tuples
    Optional(Optional),

    /// "this", see https://www.typescriptlang.org/docs/handbook/advanced-types.html#polymorphic-this-types
    This(This),

    Generic(Generic),

    /// A standalone validation expression.
    Validation(validation::Expr),

    /// A type with validation applied to it.
    Validated(Validated),

    // A custom type of some kind.
    Custom(Custom),
}

#[derive(Debug, Clone, Hash, Serialize)]
pub struct Array(pub Box<Type>);

#[derive(Debug, Clone, Hash, Serialize)]
pub struct Optional(pub Box<Type>);

#[derive(Debug, Clone, Hash, Serialize)]
pub struct Tuple {
    pub types: Vec<Type>,
}

#[derive(Debug, Clone, Hash, Serialize)]
pub struct Union {
    pub types: Vec<Type>,
}

#[derive(Debug, Clone, Hash, Serialize)]
pub struct Validated {
    pub typ: Box<Type>,
    pub expr: validation::Expr,
}

#[derive(Debug, Clone, Serialize, Hash)]
pub enum Custom {
    /// A specification of how the type should be encoded on the wire.
    WireSpec(WireSpec),
}

impl Custom {
    pub fn identical(&self, other: &Custom) -> bool {
        match (self, other) {
            (Custom::WireSpec(a), Custom::WireSpec(b)) => a.identical(b),
        }
    }
}

#[derive(Debug, Clone, Serialize, Hash)]
pub struct WireSpec {
    /// What location in the request should be used for this field.
    pub location: WireLocation,

    /// The underlying type this is.
    pub underlying: Box<Type>,

    /// Whether this specifies a name override.
    pub name_override: Option<String>,
}

#[derive(Debug, Clone, Serialize, Hash, PartialEq, Eq)]
pub enum WireLocation {
    Query,
    Header,
    PubSubAttr,
    Cookie,
}

impl WireSpec {
    pub fn identical(&self, other: &WireSpec) -> bool {
        self.location == other.location
            && self.name_override == other.name_override
            && self.underlying.identical(&other.underlying)
    }
}

impl Type {
    pub fn is_void(&self) -> bool {
        matches!(self, Type::Basic(Basic::Void))
    }
}

impl Type {
    pub fn identical(&self, other: &Type) -> bool {
        match (self, other) {
            (Type::Basic(a), Type::Basic(b)) => a == b,
            (Type::Array(a), Type::Array(b)) => a.0.identical(&b.0),
            (Type::Interface(a), Type::Interface(b)) => a.identical(b),
            (Type::Union(a), Type::Union(b)) => {
                a.types.iter().zip(&b.types).all(|(a, b)| a.identical(b))
            }
            (Type::Tuple(a), Type::Tuple(b)) => {
                a.types.iter().zip(&b.types).all(|(a, b)| a.identical(b))
            }
            (Type::Literal(a), Type::Literal(b)) => a == b,
            (Type::Class(a), Type::Class(b)) => a.identical(b),
            (Type::Named(a), Type::Named(b)) => a.identical(b),
            (Type::Optional(a), Type::Optional(b)) => a.0.identical(&b.0),
            (Type::This(This), Type::This(This)) => true,
            (Type::Generic(a), Type::Generic(b)) => a.identical(b),
            (Type::Enum(a), Type::Enum(b)) => a.identical(b),
            (Type::Custom(a), Type::Custom(b)) => a.identical(b),
            _ => false,
        }
    }

    /// Returns a union type that merges `self` and `other`, if possible.
    /// If the types cannot be merged, it returns None.
    #[tracing::instrument(ret, level = "trace")]
    pub(super) fn union_merge(&self, other: &Type) -> Option<Type> {
        match (self, other) {
            // 'any' and any type unify to 'any'.
            (Type::Basic(Basic::Any), _) | (_, Type::Basic(Basic::Any)) => {
                Some(Type::Basic(Basic::Any))
            }

            // Type literals unify with their basic type
            (Type::Basic(basic), Type::Literal(lit)) | (Type::Literal(lit), Type::Basic(basic))
                if *basic == lit.basic() =>
            {
                Some(Type::Basic(*basic))
            }

            // Unify validation.
            (Type::Validation(a), Type::Validation(b)) => {
                Some(Type::Validation(a.clone().or(b.clone())))
            }
            (Type::Validated(validated), Type::Validation(expr))
            | (Type::Validation(expr), Type::Validated(validated)) => {
                Some(Type::Validated(Validated {
                    typ: validated.typ.to_owned(),
                    expr: validated.expr.clone().or(expr.clone()),
                }))
            }

            // TODO more rules?

            // Identical types unify.
            (this, other) if this.identical(other) => Some(this.clone()),

            // Otherwise no unification is possible.
            (_, _) => None,
        }
    }

    pub(super) fn simplify_or_union(self, other: Type) -> Type {
        match self.union_merge(&other) {
            Some(typ) => typ,
            None => Type::Union(Union {
                types: vec![self, other],
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum Basic {
    Any,
    String,
    Boolean,
    Number,
    Object,
    BigInt,
    Date,
    Symbol,
    Undefined,
    Null,
    Void,
    Unknown,
    Never,
}

impl Display for Basic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s: &'static str = match self {
            Basic::Any => "any",
            Basic::String => "string",
            Basic::Boolean => "boolean",
            Basic::Number => "number",
            Basic::Object => "object",
            Basic::BigInt => "bigint",
            Basic::Date => "Date",
            Basic::Symbol => "symbol",
            Basic::Undefined => "undefined",
            Basic::Null => "null",
            Basic::Void => "void",
            Basic::Unknown => "unknown",
            Basic::Never => "never",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, Serialize)]
pub enum Literal {
    String(String),
    Boolean(bool),
    Number(f64),
    BigInt(String),
}

impl Literal {
    pub fn basic(&self) -> Basic {
        match self {
            Literal::String(_) => Basic::String,
            Literal::Boolean(_) => Basic::Boolean,
            Literal::Number(_) => Basic::Number,
            Literal::BigInt(_) => Basic::BigInt,
        }
    }
}

impl PartialEq for Literal {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Literal::String(a), Literal::String(b)) => a == b,
            (Literal::Boolean(a), Literal::Boolean(b)) => a == b,
            (Literal::Number(a), Literal::Number(b)) => a == b,
            (Literal::BigInt(a), Literal::BigInt(b)) => a == b,
            _ => false,
        }
    }
}

// Safe because the float literals don't include non-Eq values like NaN since they're literals.
impl Eq for Literal {}

impl Hash for Literal {
    fn hash<H: Hasher>(&self, state: &mut H) {
        fn integer_decode(val: f64) -> (u64, i16, i8) {
            let bits: u64 = val.to_bits();
            let sign: i8 = if bits >> 63 == 0 { 1 } else { -1 };
            let mut exponent: i16 = ((bits >> 52) & 0x7ff) as i16;
            let mantissa = if exponent == 0 {
                (bits & 0xfffffffffffff) << 1
            } else {
                (bits & 0xfffffffffffff) | 0x10000000000000
            };

            exponent -= 1023 + 52;
            (mantissa, exponent, sign)
        }

        match self {
            Literal::String(s) => s.hash(state),
            Literal::Boolean(b) => b.hash(state),
            Literal::Number(n) => {
                self.hash(state);
                integer_decode(*n).hash(state);
            }
            Literal::BigInt(s) => s.hash(state),
        }
    }
}

#[derive(Debug, Clone, Hash, Serialize)]
pub struct Interface {
    /// Explicitly defined fields.
    pub fields: Vec<InterfaceField>,

    /// Set for index signature types, like `[key: string]: number`.
    pub index: Option<(Box<Type>, Box<Type>)>,

    /// Callable signature, like `(a: number): string`.
    /// The first tuple element is the args, and the second is the returns.
    pub call: Option<(Vec<Type>, Vec<Type>)>,
}

impl Interface {
    pub fn identical(&self, other: &Interface) -> bool {
        if self.fields.len() != other.fields.len() {
            return false;
        }
        if self.index.is_some() != other.index.is_some() {
            return false;
        }

        // Collect the fields by name.
        #[allow(clippy::mutable_key_type)]
        let by_name = self
            .fields
            .iter()
            .map(|f| (f.name.clone(), f))
            .collect::<HashMap<_, _>>();

        // Check that all fields in `other` are in `self`.
        for field in &other.fields {
            if let Some(self_field) = by_name.get(&field.name) {
                if !self_field.identical(field) {
                    return false;
                }
            } else {
                return false;
            }
        }

        // Compare index signatures.
        if let (Some((self_key, self_value)), Some((other_key, other_value))) =
            (&self.index, &other.index)
        {
            if !self_key.identical(other_key) || !self_value.identical(other_value) {
                return false;
            }
        }

        true
    }
}

#[derive(Debug, Clone, Hash, Serialize, Eq, PartialEq)]
pub enum FieldName {
    String(String),
    Symbol(Rc<Object>),
}

impl FieldName {
    pub fn eq_str(&self, str: &str) -> bool {
        match self {
            FieldName::String(s) => s == str,
            FieldName::Symbol(_) => false,
        }
    }
}

#[derive(Clone, Hash, Serialize)]
pub struct InterfaceField {
    pub range: Range,
    pub name: FieldName,
    pub optional: bool,
    pub typ: Type,
}

impl Debug for InterfaceField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InterfaceField")
            .field("name", &self.name)
            .field("optional", &self.optional)
            .field("typ", &self.typ)
            .finish()
    }
}

impl InterfaceField {
    pub fn identical(&self, other: &InterfaceField) -> bool {
        self.name == other.name && self.typ.identical(&other.typ) && self.optional == other.optional
    }
}

#[derive(Debug, Clone, Hash, Serialize)]
pub struct ClassType {
    pub methods: Vec<String>,
    // TODO: include class fields here
}

impl ClassType {
    pub fn identical(&self, _other: &ClassType) -> bool {
        todo!()
    }
}

#[derive(Debug, Clone, Hash, Serialize, Eq, PartialEq)]
pub struct EnumType {
    pub members: Vec<EnumMember>,
}

#[derive(Debug, Clone, Hash, Serialize, Eq, PartialEq)]
pub struct EnumMember {
    pub name: String,
    pub value: EnumValue,
}

#[derive(Debug, Clone, Hash, Serialize, Eq, PartialEq)]
pub enum EnumValue {
    String(String),
    Number(i64),
}

impl EnumValue {
    pub fn to_literal(self) -> Literal {
        match self {
            EnumValue::String(s) => Literal::String(s),
            EnumValue::Number(n) => Literal::Number(n as f64),
        }
    }

    pub fn to_type(self) -> Type {
        Type::Literal(self.to_literal())
    }
}

impl EnumType {
    pub fn identical(&self, other: &EnumType) -> bool {
        if self.members.len() != other.members.len() {
            return false;
        }
        *self == *other
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Named {
    pub obj: Rc<object::Object>,
    pub type_arguments: Vec<Type>,
}

impl Hash for Named {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.obj.id.hash(state);
        self.type_arguments.hash(state);
    }
}

impl Named {
    pub fn new(obj: Rc<object::Object>, type_arguments: Vec<Type>) -> Self {
        Self {
            obj,
            type_arguments,
        }
    }

    pub fn identical(&self, other: &Named) -> bool {
        if self.obj.id != other.obj.id || self.type_arguments.len() != other.type_arguments.len() {
            return false;
        }

        for (a, b) in self.type_arguments.iter().zip(&other.type_arguments) {
            if !a.identical(b) {
                return false;
            }
        }
        true
    }

    pub fn underlying(&self, state: &ResolveState) -> Type {
        let ctx = Ctx::new(state, self.obj.module_id);
        ctx.underlying_named(self)
    }
}

#[derive(Debug, Clone, Hash, Serialize)]
pub enum Generic {
    /// A reference to a generic type parameter.
    TypeParam(TypeParam),

    /// An index lookup, like `T[U]`, where at least one of the types is a generic.
    Index(Index),

    /// A mapped type.
    Mapped(Mapped),

    /// A reference to the 'key' type when evaluating a mapped type.
    MappedKeyType(MappedKeyType),

    Keyof(Keyof),
    Conditional(Conditional),
    // A reference to the 'as' type when evaluating a mapped type.
    // MappedAsType,

    // An intersection type.
    Intersection(Intersection),

    /// A reference to an inferred type parameter,
    /// referencing its index in infer_type_params.
    Inferred(Inferred),
}

#[derive(Debug, Clone, Hash, Serialize)]
pub struct Index {
    pub source: Box<Type>,
    pub index: Box<Type>,
}

/// A reference to an inferred type parameter,
/// referencing its index in infer_type_params.
#[derive(Debug, Clone, Hash, Serialize)]
pub struct Inferred(pub usize);

#[derive(Debug, Clone, Hash, Serialize)]
pub struct Keyof(pub Box<Type>);

#[derive(Debug, Clone, Hash, Serialize)]
pub struct This;

#[derive(Debug, Clone, Hash, Serialize)]
pub struct MappedKeyType;

#[derive(Debug, Clone, Hash, Serialize)]
pub struct TypeParam {
    // The index of the type parameter in the current scope.
    pub idx: usize,

    // Any additional constraint on the type parameter.
    // If provided, it can be assumed that the type parameter is assignable to this type.
    pub constraint: Option<Box<Type>>,
}

impl TypeParam {
    pub fn identical(&self, other: &TypeParam) -> bool {
        self.idx == other.idx
            && match (self.constraint.as_ref(), other.constraint.as_ref()) {
                (Some(a), Some(b)) => a.identical(b),
                (None, None) => true,
                _ => false,
            }
    }
}

#[derive(Debug, Clone, Hash, Serialize)]
pub struct Mapped {
    /// The type being evaluated to find property names.
    /// Must be evaluated using the property name in the evaluation context.
    pub in_type: Box<Type>,

    /// The value of each property in the mapped type.
    /// Must be evaluated using the property name in the evaluation context.
    pub value_type: Box<Type>,

    /// Whether to force fields to be optional (Some(True)), to make them required (Some(False)),
    /// or to keep them as-is (None).
    pub optional: Option<bool>,
    // Indicates a remapping of the property name.
    // Must be evaluated using the property name in the evaluation context.
    // pub as_type: Option<Box<Type>>,
}

impl Mapped {
    pub fn identical(&self, other: &Mapped) -> bool {
        self.in_type.identical(&other.in_type) && self.value_type.identical(&other.value_type)
    }
}

#[derive(Debug, Clone, Hash, Serialize)]
pub struct Intersection {
    pub x: Box<Type>,
    pub y: Box<Type>,
}

impl Intersection {
    pub fn identical(&self, other: &Intersection) -> bool {
        self.x.identical(&other.x) && self.y.identical(&other.y)
    }
}

#[derive(Debug, Clone, Hash, Serialize)]
pub struct Conditional {
    pub check_type: Box<Type>,
    pub extends_type: Box<Type>,
    pub true_type: Box<Type>,
    pub false_type: Box<Type>,
}

impl Generic {
    pub fn identical(&self, other: &Generic) -> bool {
        match (self, other) {
            (Generic::TypeParam(a), Generic::TypeParam(b)) => a.identical(b),
            (Generic::Mapped(a), Generic::Mapped(b)) => a.identical(b),
            _ => false,
        }
    }
}

impl Type {
    pub fn iter_unions<'a>(&'a self) -> Box<dyn Iterator<Item = &'a Type> + 'a> {
        match self {
            Type::Union(union) => Box::new(union.types.iter().flat_map(|t| t.iter_unions())),
            Type::Optional(tt) => Box::new(
                tt.0.iter_unions()
                    .chain(std::iter::once(&Type::Basic(Basic::Undefined))),
            ),
            Type::Validated(v) => v.typ.iter_unions(),
            _ => Box::new(std::iter::once(self)),
        }
    }

    pub fn into_iter_unions(self) -> Box<dyn Iterator<Item = Type>> {
        match self {
            Type::Union(union) => {
                Box::new(union.types.into_iter().flat_map(|t| t.into_iter_unions()))
            }
            Type::Optional(tt) => Box::new(
                tt.0.into_iter_unions()
                    .chain(std::iter::once(Type::Basic(Basic::Undefined))),
            ),
            Type::Validated(v) => v.typ.into_iter_unions(),
            _ => Box::new(std::iter::once(self)),
        }
    }
}

impl Type {
    /// Reports whether `self` is assignable to `other`.
    /// If the result is indeterminate due to an unresolved type, it reports None.
    pub fn assignable(&self, state: &ResolveState, other: &Type) -> Option<bool> {
        match (self, other) {
            (_, Type::Basic(Basic::Any)) => Some(true),
            (_, Type::Basic(Basic::Never)) => Some(false),
            (Type::Generic(_), _) | (_, Type::Generic(_)) => None,

            // Unwrap named types.
            (Type::Named(a), b) => {
                let a = a.underlying(state);
                a.assignable(state, b)
            }
            (a, Type::Named(b)) => {
                let b = b.underlying(state);
                a.assignable(state, &b)
            }

            (Type::Basic(a), Type::Basic(b)) => Some(a == b),
            (Type::Literal(a), Type::Basic(b)) => Some(matches!(
                (a, b),
                (_, Basic::Any)
                    | (Literal::String(_), Basic::String)
                    | (Literal::Boolean(_), Basic::Boolean)
                    | (Literal::Number(_), Basic::Number)
                    | (Literal::BigInt(_), Basic::BigInt)
            )),

            (Type::Validated(v), _) | (_, Type::Validated(v)) => v.typ.assignable(state, other),

            (this, Type::Optional(other)) => {
                if matches!(this, Type::Basic(Basic::Undefined)) {
                    Some(true)
                } else {
                    this.assignable(state, &other.0)
                }
            }

            (Type::Tuple(this), other) => match other {
                Type::Tuple(other) => {
                    if this.types.len() != other.types.len() {
                        return Some(false);
                    }

                    let mut found_none = false;
                    for (this, other) in this.types.iter().zip(&other.types) {
                        match this.assignable(state, other) {
                            Some(true) => {}
                            Some(false) => return Some(false),
                            None => found_none = true,
                        }
                    }
                    if found_none {
                        None
                    } else {
                        Some(true)
                    }
                }

                Type::Array(other) => {
                    // Ensure every element in `this` is a subtype of `other`.
                    for this in &this.types {
                        match this.assignable(state, &other.0) {
                            Some(true) => {}
                            Some(false) => return Some(false),
                            None => return None,
                        }
                    }
                    Some(true)
                }
                _ => Some(false),
            },

            (Type::Enum(a), other) => {
                let this_fields: HashMap<&str, &EnumValue> =
                    HashMap::from_iter(a.members.iter().map(|m| (m.name.as_str(), &m.value)));
                match other {
                    Type::Enum(other) => {
                        // Does every field in `other` exist in `this_fields`?
                        for mem in &other.members {
                            if let Some(this_field) = this_fields.get(mem.name.as_str()) {
                                if **this_field == mem.value {
                                    continue;
                                }
                            }
                            return Some(false);
                        }
                        Some(true)
                    }

                    Type::Interface(other) => {
                        // Does every field in `other` exist in `iface`?
                        let mut found_none = false;
                        for field in &other.fields {
                            if let FieldName::String(name) = &field.name {
                                if let Some(this_field) = this_fields.get(name.as_str()) {
                                    let this_typ = (*this_field).clone().to_type();
                                    match this_typ.assignable(state, &field.typ) {
                                        Some(true) => continue,
                                        Some(false) => return Some(false),
                                        None => found_none = true,
                                    }
                                }
                            }
                        }
                        if found_none {
                            None
                        } else {
                            Some(true)
                        }
                    }
                    _ => Some(false),
                }
            }

            (Type::Interface(iface), other) => {
                #[allow(clippy::mutable_key_type)]
                let this_fields: HashMap<&FieldName, &InterfaceField> =
                    HashMap::from_iter(iface.fields.iter().map(|f| (&f.name, f)));
                match other {
                    Type::Interface(other) => {
                        // Does every field in `other` exist in `iface`?
                        let mut found_none = false;
                        for field in &other.fields {
                            if let Some(this_field) = this_fields.get(&field.name) {
                                match this_field.typ.assignable(state, &field.typ) {
                                    Some(true) => {}
                                    Some(false) => return Some(false),
                                    None => found_none = true,
                                }
                            } else {
                                return Some(false);
                            }
                        }
                        if found_none {
                            None
                        } else {
                            Some(true)
                        }
                    }
                    _ => Some(false),
                }
            }

            (this, Type::Union(other)) => {
                // Is every element in `this` assignable to `other`?
                'ThisLoop: for t in this.iter_unions() {
                    let mut found_none = false;
                    for o in &other.types {
                        match t.assignable(state, o) {
                            // Found a match; check the next element in `this`.
                            Some(true) => continue 'ThisLoop,

                            // Not a match; keep going.
                            Some(false) => {}
                            None => found_none = true,
                        }
                    }

                    // Couldn't find any match
                    return if found_none { None } else { Some(false) };
                }

                // All elements passed the test.
                Some(true)
            }

            (a, b) => Some(a.identical(b)),
        }
    }
}

pub enum Extends<'a> {
    Yes(Vec<(usize, Cow<'a, Type>)>),
    No,
    Unknown,
}

impl Extends<'_> {
    pub fn into_static(self) -> Extends<'static> {
        match self {
            Extends::Yes(v) => Extends::Yes(
                v.into_iter()
                    .map(|(idx, t)| (idx, Cow::Owned(t.into_owned())))
                    .collect(),
            ),
            Extends::No => Extends::No,
            Extends::Unknown => Extends::Unknown,
        }
    }
}

impl Type {
    /// Reports whether `self` is assignable to `other`.
    /// If the result is indeterminate due to an unresolved type, it reports None.
    pub fn extends<'a>(&'a self, state: &'_ ResolveState, other: &'_ Type) -> Extends<'a> {
        use Extends::*;

        fn empty_yes_or_no(val: bool) -> Extends<'static> {
            if val {
                Yes(vec![])
            } else {
                No
            }
        }

        match (self, other) {
            (this, Type::Generic(Generic::Inferred(inferred))) => {
                Yes(vec![(inferred.0, Cow::Borrowed(this))])
            }

            (_, Type::Basic(Basic::Any)) => Yes(vec![]),
            (_, Type::Basic(Basic::Never)) => No,
            (Type::Generic(_), _) | (_, Type::Generic(_)) => Unknown,

            // Unwrap named types.
            (Type::Named(a), b) => {
                let a = a.underlying(state);
                a.extends(state, b).into_static()
            }
            (a, Type::Named(b)) => {
                let b = b.underlying(state);
                a.extends(state, &b)
            }

            (Type::Basic(a), Type::Basic(b)) => empty_yes_or_no(a == b),
            (Type::Literal(a), Type::Basic(b)) => empty_yes_or_no(matches!(
                (a, b),
                (_, Basic::Any)
                    | (Literal::String(_), Basic::String)
                    | (Literal::Boolean(_), Basic::Boolean)
                    | (Literal::Number(_), Basic::Number)
                    | (Literal::BigInt(_), Basic::BigInt)
            )),

            (Type::Validated(v), _) | (_, Type::Validated(v)) => {
                v.typ.extends(state, other).into_static()
            }

            (this, Type::Optional(other)) => {
                if matches!(this, Type::Basic(Basic::Undefined)) {
                    Yes(vec![])
                } else {
                    this.extends(state, &other.0)
                }
            }

            (Type::Tuple(this), other) => match other {
                Type::Tuple(other) => {
                    if this.types.len() != other.types.len() {
                        return No;
                    }

                    let mut found_unknown = false;
                    let mut inferred = vec![];
                    for (this, other) in this.types.iter().zip(&other.types) {
                        match this.extends(state, other) {
                            Yes(inf) => {
                                inferred.extend(inf);
                            }
                            No => return No,
                            Unknown => found_unknown = true,
                        }
                    }
                    if found_unknown {
                        Unknown
                    } else {
                        Yes(inferred)
                    }
                }

                Type::Array(other) => {
                    // Ensure every element in `this` is a subtype of `other`.
                    let mut inferred = vec![];
                    for this in &this.types {
                        match this.extends(state, &other.0) {
                            Yes(infer) => inferred.extend(infer),
                            No => return No,
                            Unknown => return Unknown,
                        }
                    }

                    // Since `this` is a tuple but `other` is a single array type,
                    // it's possible we'll have multiple inferred types for the same index.
                    // Group them by index and turn them into a union type if necessary.
                    inferred.sort_by_key(|(idx, _)| *idx);

                    let inferred = inferred
                        .into_iter()
                        .chunk_by(|(idx, _)| *idx)
                        .into_iter()
                        .map(|(idx, types)| {
                            let types = types.map(|(_, t)| t.into_owned()).collect();
                            let typ = simplify_union(types);
                            (idx, Cow::Owned(typ))
                        })
                        .collect();

                    Yes(inferred)
                }
                _ => No,
            },

            (Type::Enum(a), other) => {
                let this_fields: HashMap<&str, &EnumValue> =
                    HashMap::from_iter(a.members.iter().map(|m| (m.name.as_str(), &m.value)));
                match other {
                    Type::Enum(other) => {
                        // Does every field in `other` exist in `this_fields`?
                        for mem in &other.members {
                            if let Some(this_field) = this_fields.get(mem.name.as_str()) {
                                if **this_field == mem.value {
                                    continue;
                                }
                            }
                            return No;
                        }
                        Yes(vec![])
                    }

                    Type::Interface(other) => {
                        // Does every field in `other` exist in `iface`?
                        let mut found_none = false;
                        for field in &other.fields {
                            if let FieldName::String(name) = &field.name {
                                if let Some(this_field) = this_fields.get(name.as_str()) {
                                    let this_typ = (*this_field).clone().to_type();
                                    match this_typ.assignable(state, &field.typ) {
                                        Some(true) => continue,
                                        Some(false) => return No,
                                        None => found_none = true,
                                    }
                                }
                            }
                        }
                        if found_none {
                            Unknown
                        } else {
                            Yes(vec![])
                        }
                    }
                    _ => No,
                }
            }

            (Type::Interface(iface), other) => {
                #[allow(clippy::mutable_key_type)]
                let this_fields: HashMap<&FieldName, &InterfaceField> =
                    HashMap::from_iter(iface.fields.iter().map(|f| (&f.name, f)));
                match other {
                    Type::Interface(other) => {
                        // Does every field in `other` exist in `iface`?
                        let mut found_unknown = false;
                        let mut inferred = vec![];
                        for field in &other.fields {
                            if let Some(this_field) = this_fields.get(&field.name) {
                                match this_field.typ.extends(state, &field.typ) {
                                    Yes(inf) => inferred.extend(inf),
                                    No => return No,
                                    Unknown => found_unknown = true,
                                }
                            } else {
                                return No;
                            }
                        }
                        if found_unknown {
                            Unknown
                        } else {
                            Yes(inferred)
                        }
                    }
                    _ => No,
                }
            }

            (this, Type::Union(other)) => {
                // Is every element in `this` assignable to `other`?
                let mut found_yes = false;
                let mut found_unknown = false;
                let mut inferred = Vec::new();
                for t in this.iter_unions() {
                    for o in &other.types {
                        match t.extends(state, o) {
                            Yes(inf) => {
                                found_yes = true;
                                inferred.extend(inf);
                            }
                            No => {}
                            Unknown => found_unknown = true,
                        }
                    }
                }

                if found_yes {
                    Yes(inferred)
                } else if found_unknown {
                    Unknown
                } else {
                    No
                }
            }

            (a, b) => empty_yes_or_no(a.identical(b)),
        }
    }
}

pub fn simplify_union(types: Vec<Type>) -> Type {
    let mut results: Vec<Type> = Vec::with_capacity(types.len());

    for typ in types {
        // Ignore `never` in unions.
        if matches!(typ, Type::Basic(Basic::Never)) {
            continue;
        }

        let mut found = false;
        for unified_typ in &mut results {
            match unified_typ.union_merge(&typ) {
                Some(u) => {
                    *unified_typ = u;
                    found = true;
                    break;
                }
                None => {
                    // No unification possible; keep going.
                }
            }
        }

        if !found {
            results.push(typ);
        }
    }

    match results.len() {
        0 => Type::Basic(Basic::Never),
        1 => results.remove(0),
        _ => Type::Union(Union { types: results }),
    }
}

/// Computes (a & b), the intersection of two types.
#[tracing::instrument(level = "trace", skip(ctx), ret)]
pub fn intersect<'a: 'b, 'b>(
    ctx: &'b Ctx<'a>,
    a: Cow<'a, Type>,
    b: Cow<'a, Type>,
) -> Cow<'a, Type> {
    let union_with = |a: Cow<'_, Type>, b: Cow<'_, Type>| {
        let result = a
            .into_owned()
            .into_iter_unions()
            .filter_map(
                |typ| match intersect(ctx, Cow::Owned(typ), b.clone()).into_owned() {
                    Type::Basic(Basic::Never) => None,
                    other => Some(other),
                },
            )
            .collect();
        Cow::Owned(simplify_union(result))
    };
    let literal = |lit: &Literal, b: &Basic| -> bool {
        matches!(
            (lit, b),
            (
                Literal::String(_),
                Basic::String | Basic::Any | Basic::Unknown
            ) | (
                Literal::Boolean(_),
                Basic::Boolean | Basic::Any | Basic::Unknown
            ) | (
                Literal::Number(_),
                Basic::Number | Basic::Any | Basic::Unknown
            ) | (
                Literal::BigInt(_),
                Basic::BigInt | Basic::Any | Basic::Unknown
            )
        )
    };

    match (a.as_ref(), b.as_ref()) {
        // T & unknown == T
        (Type::Basic(Basic::Unknown), _) => b,
        (_, Type::Basic(Basic::Unknown)) => a,

        // T & any == any
        (Type::Basic(Basic::Any), _) | (_, Type::Basic(Basic::Any)) => {
            Cow::Owned(Type::Basic(Basic::Any))
        }

        // T & never == never
        (Type::Basic(Basic::Never), _) | (_, Type::Basic(Basic::Never)) => {
            Cow::Owned(Type::Basic(Basic::Never))
        }

        (Type::Basic(a), Type::Basic(b)) => {
            if a == b {
                Cow::Owned(Type::Basic(*a))
            } else {
                Cow::Owned(Type::Basic(Basic::Never))
            }
        }

        // Intersection distributes into unions.
        (Type::Union(a), Type::Union(b)) => {
            let mut types = Vec::with_capacity(a.types.len() * b.types.len());
            for typ in &a.types {
                for other in &b.types {
                    match intersect(ctx, Cow::Borrowed(typ), Cow::Borrowed(other)).into_owned() {
                        Type::Basic(Basic::Never) => {}
                        other => types.push(other),
                    }
                }
            }
            Cow::Owned(simplify_union(types))
        }
        (Type::Union(_), _) => union_with(a, b),
        (_, Type::Union(_)) => union_with(b, a),

        (Type::Literal(x), Type::Literal(y)) if x == y => a,
        (Type::Literal(lit), Type::Basic(x)) => {
            if literal(lit, x) {
                a
            } else {
                Cow::Owned(Type::Basic(Basic::Never))
            }
        }
        (Type::Basic(x), Type::Literal(lit)) => {
            if literal(lit, x) {
                b
            } else {
                Cow::Owned(Type::Basic(Basic::Never))
            }
        }

        (Type::Array(x), Type::Array(y)) => Cow::Owned(Type::Array(Array(Box::new(
            intersect(
                ctx,
                Cow::Borrowed(x.0.as_ref()),
                Cow::Borrowed(y.0.as_ref()),
            )
            .into_owned(),
        )))),
        (Type::Array(x), Type::Tuple(y)) | (Type::Tuple(y), Type::Array(x)) => {
            Cow::Owned(Type::Array(Array(Box::new(if y.types.is_empty() {
                Type::Basic(Basic::Never)
            } else {
                // Inspect the first element of the tuple for intersection.
                // It's not completely correct but close enough for now.
                intersect(ctx, Cow::Borrowed(x.0.as_ref()), Cow::Borrowed(&y.types[0])).into_owned()
            }))))
        }

        (Type::Tuple(x), Type::Tuple(y)) => {
            let mut types = Vec::with_capacity(x.types.len().min(y.types.len()));
            for (a, b) in x.types.iter().zip(y.types.iter()) {
                types.push(intersect(ctx, Cow::Borrowed(a), Cow::Borrowed(b)).into_owned());
            }
            Cow::Owned(Type::Tuple(Tuple { types }))
        }

        (Type::Optional(x), Type::Optional(y)) => Cow::Owned(Type::Optional(Optional(Box::new(
            intersect(ctx, Cow::Borrowed(&x.0), Cow::Borrowed(&y.0)).into_owned(),
        )))),
        // Treat optional as "T | undefined".
        (Type::Optional(x), y) | (y, Type::Optional(x)) => {
            union_with(Cow::Borrowed(&x.0), Cow::Borrowed(y))
        }

        (Type::This(This), Type::This(This)) => Cow::Owned(Type::This(This)),

        // Combine validation expressions into a validated type.
        (Type::Validated(a), Type::Validation(b)) => Cow::Owned(Type::Validated(Validated {
            typ: a.typ.clone(),
            expr: a.expr.clone().and(b.clone()),
        })),
        (Type::Validation(a), Type::Validated(b)) => Cow::Owned(Type::Validated(Validated {
            typ: b.typ.clone(),
            expr: a.clone().and(b.expr.clone()),
        })),

        // Merge validation expressions together.
        (Type::Validation(a), Type::Validation(b)) => {
            Cow::Owned(Type::Validation(a.clone().and(b.clone())))
        }
        (_, Type::Validation(expr)) => Cow::Owned(Type::Validated(Validated {
            typ: Box::new(a.into_owned()),
            expr: expr.clone(),
        })),
        (Type::Validation(expr), _) => Cow::Owned(Type::Validated(Validated {
            typ: Box::new(b.into_owned()),
            expr: expr.clone(),
        })),

        (Type::Generic(_), _) | (_, Type::Generic(_)) => {
            Cow::Owned(Type::Generic(Generic::Intersection(Intersection {
                x: Box::new(a.into_owned()),
                y: Box::new(b.into_owned()),
            })))
        }

        (Type::Class(_), Type::Class(_)) => {
            Cow::Owned(Type::Generic(Generic::Intersection(Intersection {
                x: Box::new(a.into_owned()),
                y: Box::new(b.into_owned()),
            })))
        }

        (Type::Named(x), _) => {
            let x = ctx.underlying_named(x);
            intersect(ctx, Cow::Owned(x), b)
        }
        (_, Type::Named(y)) => {
            let y = ctx.underlying_named(y);
            intersect(ctx, a, Cow::Owned(y))
        }

        (Type::Interface(_), Type::Interface(_)) => {
            let Type::Interface(x) = a.into_owned() else {
                unreachable!();
            };
            let Type::Interface(y) = b.into_owned() else {
                unreachable!();
            };

            let fields = {
                let mut y_fields = y
                    .fields
                    .into_iter()
                    .map(|f| (f.name.clone(), Some(f)))
                    .collect::<IndexMap<_, _>>();

                // Fields are added together.
                // If they have the same name, the type is the intersection.
                let mut result = Vec::with_capacity(x.fields.len() + y_fields.len());

                for mut field in x.fields {
                    if let Some(other) = y_fields.get_mut(&field.name) {
                        // Intersect the type.
                        let other = other.take().expect("field name should not appear twice");
                        field.typ = intersect(ctx, Cow::Owned(field.typ), Cow::Owned(other.typ))
                            .into_owned();
                        field.optional = field.optional && other.optional;
                    }
                    result.push(field);
                }

                // Add any remaining fields from `y`.
                for (_, other) in y_fields {
                    if let Some(other) = other {
                        result.push(other);
                    }
                }
                result
            };

            // If we have any fields, ignore the index signature.
            let index = if fields.is_empty() {
                None
            } else {
                match (x.index, y.index) {
                    (Some((x_key, x_value)), Some((y_key, y_value))) => {
                        let key =
                            intersect(ctx, Cow::Owned(*x_key), Cow::Owned(*y_key)).into_owned();
                        let value =
                            intersect(ctx, Cow::Owned(*x_value), Cow::Owned(*y_value)).into_owned();
                        Some((Box::new(key), Box::new(value)))
                    }
                    (Some((k, v)), None) | (None, Some((k, v))) => Some((k, v)),
                    (None, None) => None,
                }
            };

            if x.call.is_some() || y.call.is_some() {
                HANDLER.with(|handler| {
                    handler.err("intersection of call signature types not yet supported");
                })
            }

            Cow::Owned(Type::Interface(Interface {
                fields,
                index,
                call: None,
            }))
        }

        (Type::Interface(_), _) | (_, Type::Interface(_)) => {
            Cow::Owned(Type::Generic(Generic::Intersection(Intersection {
                x: Box::new(a.into_owned()),
                y: Box::new(b.into_owned()),
            })))
        }

        (_, _) => Cow::Owned(Type::Basic(Basic::Never)),
    }
}

pub fn unwrap_validated(typ: &Type) -> (&Type, Option<&validation::Expr>) {
    match typ {
        Type::Validated(validated) => (&validated.typ, Some(&validated.expr)),
        _ => (typ, None),
    }
}
