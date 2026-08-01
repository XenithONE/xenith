//! Type representation.
//!
//! Xenith annotates every parameter, return type and field, so the checker
//! needs no unification variables and no constraint solver. Types are
//! constructed directly from what the source says.
//!
//! Three kinds warrant attention, because conflating them is how a checker
//! either drowns the reader in cascades or goes silent on real problems:
//!
//! - [`Type::Error`] — recovery. Compatible with everything, reported never.
//! - [`Type::Hole`] — a deliberate gap. **Not** an error: it carries a goal.
//! - Everything else — an actual type.

use std::fmt;

/// Identifies a declaration: a struct, an enum, or a built-in type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DefId(pub u32);

/// Identifies one hole within a module, in source order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HoleId(pub u32);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Type {
    Int,
    Float,
    Bool,
    Text,
    Char,
    Unit,

    /// A declared type applied to arguments: `Player`, `Result<Config, Error>`.
    Named {
        def: DefId,
        args: Vec<Type>,
    },

    /// A type parameter in scope: the `T` inside `fn first<T>(items: List<T>)`.
    Param(String),

    Fn {
        params: Vec<Type>,
        ret: Box<Type>,
        effects: EffectSet,
    },

    /// The type of a hole. Compatible with everything so that checking
    /// continues around it, but recorded rather than reported: a partial
    /// program is a normal state, and a hole is a goal, not a mistake.
    Hole(HoleId),

    /// Recovery. Compatible with everything and never itself reported, so one
    /// mistake produces one diagnostic instead of an avalanche.
    Error,
}

impl Type {
    /// Whether this type carries no information — either recovery poison or an
    /// unfilled hole. Operations involving one of these stay silent.
    pub fn is_unknown(&self) -> bool {
        matches!(self, Type::Error | Type::Hole(_))
    }

    /// Structural compatibility, treating unknowns as compatible with anything.
    ///
    /// This is deliberately *not* a subtyping relation: Xenith has no implicit
    /// conversions, so `Int` and `Float` are unrelated and a mismatch between
    /// them is always an error.
    pub fn is_compatible_with(&self, other: &Type) -> bool {
        if self.is_unknown() || other.is_unknown() {
            return true;
        }
        match (self, other) {
            (Type::Named { def: a, args: xs }, Type::Named { def: b, args: ys }) => {
                a == b
                    && xs.len() == ys.len()
                    && xs.iter().zip(ys).all(|(x, y)| x.is_compatible_with(y))
            }
            (
                Type::Fn {
                    params: p1,
                    ret: r1,
                    effects: e1,
                },
                Type::Fn {
                    params: p2,
                    ret: r2,
                    effects: e2,
                },
            ) => {
                p1.len() == p2.len()
                    && p1.iter().zip(p2).all(|(x, y)| x.is_compatible_with(y))
                    && r1.is_compatible_with(r2)
                    // A function may be used where fewer effects are permitted
                    // only if it performs no more than those.
                    && e1.is_subset_of(e2)
            }
            (a, b) => a == b,
        }
    }

    /// Substitute type parameters by name. Used to instantiate a generic
    /// signature at a call site.
    pub fn substitute(&self, bindings: &[(String, Type)]) -> Type {
        match self {
            Type::Param(name) => bindings
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, t)| t.clone())
                .unwrap_or_else(|| self.clone()),
            Type::Named { def, args } => Type::Named {
                def: *def,
                args: args.iter().map(|a| a.substitute(bindings)).collect(),
            },
            Type::Fn {
                params,
                ret,
                effects,
            } => Type::Fn {
                params: params.iter().map(|p| p.substitute(bindings)).collect(),
                ret: Box::new(ret.substitute(bindings)),
                effects: effects.clone(),
            },
            other => other.clone(),
        }
    }
}

/// A closed set of effects, held sorted and deduplicated so that comparison and
/// display are deterministic.
///
/// Absent on a signature means *empty*, which is the strongest claim a function
/// can make: it performs no effects at all.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EffectSet {
    effects: Vec<String>,
}

impl EffectSet {
    pub const fn empty() -> EffectSet {
        EffectSet {
            effects: Vec::new(),
        }
    }

    pub fn new(items: impl IntoIterator<Item = String>) -> EffectSet {
        items.into_iter().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.effects.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.effects.iter().map(String::as_str)
    }

    pub fn contains(&self, effect: &str) -> bool {
        self.effects.iter().any(|e| e == effect)
    }

    /// Whether every effect here is permitted by `budget`.
    ///
    /// This is the check that makes a signature honest: a function may only
    /// perform what it declared, and a callee may only perform what its caller
    /// allows.
    pub fn is_subset_of(&self, budget: &EffectSet) -> bool {
        self.effects.iter().all(|e| budget.contains(e))
    }

    /// Effects present here but missing from `budget` — what a signature would
    /// have to gain to permit this.
    pub fn missing_from(&self, budget: &EffectSet) -> Vec<&str> {
        self.effects
            .iter()
            .filter(|e| !budget.contains(e))
            .map(String::as_str)
            .collect()
    }

    pub fn union(&self, other: &EffectSet) -> EffectSet {
        EffectSet::new(self.effects.iter().chain(&other.effects).cloned())
    }
}

impl FromIterator<String> for EffectSet {
    /// Sorts and deduplicates, so that two sets built in different orders
    /// compare equal and render identically.
    fn from_iter<I: IntoIterator<Item = String>>(items: I) -> EffectSet {
        let mut effects: Vec<String> = items.into_iter().collect();
        effects.sort();
        effects.dedup();
        EffectSet { effects }
    }
}

impl fmt::Display for EffectSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{{{}}}", self.effects.join(", "))
    }
}

/// Renders a type the way the source would spell it, so a diagnostic and the
/// code it refers to agree.
///
/// `Named` types need the definition table to recover their name, so this is a
/// view rather than a `Display` implementation.
pub struct TypeName<'a> {
    pub ty: &'a Type,
    pub name_of: &'a dyn Fn(DefId) -> String,
}

impl fmt::Display for TypeName<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let nested = |ty: &Type| -> String {
            // Reborrow through the same naming function.
            TypeName {
                ty,
                name_of: self.name_of,
            }
            .to_string()
        };
        match self.ty {
            Type::Int => f.write_str("Int"),
            Type::Float => f.write_str("Float"),
            Type::Bool => f.write_str("Bool"),
            Type::Text => f.write_str("Text"),
            Type::Char => f.write_str("Char"),
            Type::Unit => f.write_str("()"),
            Type::Param(name) => f.write_str(name),
            Type::Named { def, args } => {
                f.write_str(&(self.name_of)(*def))?;
                if !args.is_empty() {
                    let rendered: Vec<String> = args.iter().map(nested).collect();
                    write!(f, "<{}>", rendered.join(", "))?;
                }
                Ok(())
            }
            Type::Fn {
                params,
                ret,
                effects,
            } => {
                let rendered: Vec<String> = params.iter().map(nested).collect();
                write!(f, "fn({}) -> {}", rendered.join(", "), nested(ret))?;
                if !effects.is_empty() {
                    write!(f, " uses {effects}")?;
                }
                Ok(())
            }
            Type::Hole(_) => f.write_str("??"),
            Type::Error => f.write_str("<unknown>"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(id: u32, args: Vec<Type>) -> Type {
        Type::Named {
            def: DefId(id),
            args,
        }
    }

    #[test]
    fn unknown_types_are_compatible_with_everything() {
        // One mistake should produce one diagnostic, not an avalanche.
        assert!(Type::Error.is_compatible_with(&Type::Int));
        assert!(Type::Int.is_compatible_with(&Type::Error));
        assert!(Type::Hole(HoleId(0)).is_compatible_with(&Type::Bool));
        assert!(Type::Bool.is_compatible_with(&Type::Hole(HoleId(0))));
    }

    #[test]
    fn unrelated_primitives_are_incompatible() {
        // No implicit conversions, so these are simply different types.
        assert!(!Type::Int.is_compatible_with(&Type::Float));
        assert!(!Type::Bool.is_compatible_with(&Type::Int));
    }

    #[test]
    fn generic_arguments_must_agree() {
        assert!(named(1, vec![Type::Int]).is_compatible_with(&named(1, vec![Type::Int])));
        assert!(!named(1, vec![Type::Int]).is_compatible_with(&named(1, vec![Type::Bool])));
        assert!(!named(1, vec![Type::Int]).is_compatible_with(&named(2, vec![Type::Int])));
    }

    #[test]
    fn a_function_may_be_used_where_fewer_effects_are_permitted_only_if_it_performs_fewer() {
        let reads = EffectSet::new(["Fs.read".to_string()]);
        let reads_and_writes = EffectSet::new(["Fs.read".to_string(), "Fs.write".to_string()]);

        let pure_fn = |effects: EffectSet| Type::Fn {
            params: vec![],
            ret: Box::new(Type::Unit),
            effects,
        };

        assert!(pure_fn(reads.clone()).is_compatible_with(&pure_fn(reads_and_writes.clone())));
        assert!(!pure_fn(reads_and_writes).is_compatible_with(&pure_fn(reads)));
        assert!(
            pure_fn(EffectSet::empty())
                .is_compatible_with(&pure_fn(EffectSet::new(["Fs.read".to_string()])))
        );
    }

    #[test]
    fn effect_sets_are_order_independent() {
        let a = EffectSet::new(["Net.send".to_string(), "Fs.read".to_string()]);
        let b = EffectSet::new(["Fs.read".to_string(), "Net.send".to_string()]);
        assert_eq!(a, b);
        assert_eq!(a.to_string(), "{Fs.read, Net.send}");
    }

    #[test]
    fn duplicate_effects_collapse() {
        let set = EffectSet::new(["Fs.read".to_string(), "Fs.read".to_string()]);
        assert_eq!(set.iter().count(), 1);
    }

    #[test]
    fn missing_effects_name_what_a_signature_would_have_to_gain() {
        let needed = EffectSet::new(["Fs.read".to_string(), "Net.send".to_string()]);
        let budget = EffectSet::new(["Fs.read".to_string()]);
        assert_eq!(needed.missing_from(&budget), ["Net.send"]);
        assert!(!needed.is_subset_of(&budget));
    }

    #[test]
    fn an_empty_effect_set_is_a_subset_of_anything() {
        let any = EffectSet::new(["Fs.read".to_string()]);
        assert!(EffectSet::empty().is_subset_of(&any));
        assert!(EffectSet::empty().is_subset_of(&EffectSet::empty()));
    }

    #[test]
    fn substitution_replaces_parameters_throughout() {
        let generic = named(1, vec![Type::Param("T".into()), Type::Int]);
        let concrete = generic.substitute(&[("T".to_string(), Type::Bool)]);
        assert_eq!(concrete, named(1, vec![Type::Bool, Type::Int]));
    }

    #[test]
    fn substitution_leaves_unbound_parameters_alone() {
        let generic = Type::Param("U".into());
        assert_eq!(
            generic.substitute(&[("T".to_string(), Type::Bool)]),
            Type::Param("U".into())
        );
    }

    #[test]
    fn type_names_render_the_way_the_source_spells_them() {
        let name_of = |id: DefId| match id.0 {
            1 => "Result".to_string(),
            2 => "List".to_string(),
            _ => "?".to_string(),
        };
        let ty = named(1, vec![named(2, vec![Type::Int]), Type::Text]);
        let rendered = TypeName {
            ty: &ty,
            name_of: &name_of,
        }
        .to_string();
        assert_eq!(rendered, "Result<List<Int>, Text>");
    }
}
