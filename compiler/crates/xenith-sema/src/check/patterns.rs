use xenith_diag::DiagCode;
use xenith_syntax::ast;

use crate::def::DefKind;
use crate::ty::Type;

use super::Checker;
use super::resolve::QualifiedLookup;

impl<'a> Checker<'a> {
    // ----- patterns -----

    pub(super) fn bind_pattern(&mut self, pattern: &ast::Pattern, scrutinee: &Type, mutable: bool) {
        match &pattern.kind {
            ast::PatternKind::Wildcard | ast::PatternKind::Error => {}

            ast::PatternKind::Binding(ident) => {
                // A lowercase name that happens to be a variant of the
                // scrutinee's enum is a variant pattern, not a binding —
                // otherwise a misspelt `None` would silently match everything.
                if let Type::Named { def, .. } = scrutinee {
                    if let Some(variant) = self.defs.variant_named(*def, &ident.name) {
                        if !variant.payload.is_empty() {
                            let message = format!(
                                "variant `{}` carries a payload; match it as `{}(..)`",
                                ident.name, ident.name
                            );
                            self.error(DiagCode::WrongArgumentCount, ident.span, message);
                        }
                        return;
                    }
                }
                // `type-at` on a binding name answers with the bound type —
                // the most natural question to ask about a `let`.
                self.maybe_probe(ident.span, scrutinee);
                self.bind(&ident.name, scrutinee.clone(), mutable);
            }

            ast::PatternKind::Literal(expr) => {
                let ty = self.synth(expr);
                self.require_compatible(&ty, scrutinee, pattern.span);
            }

            ast::PatternKind::Path(path) => {
                // `game.player.Rank.Gold` — the module prefix resolves
                // first, the enum and variant follow as before.
                if path.segments.len() >= 3 && self.ctx.is_some() {
                    let names: Vec<String> = path.segments.iter().map(|s| s.name.clone()).collect();
                    match self.qualified_ref(&names, pattern.span) {
                        QualifiedLookup::Variant(def, _) => {
                            let pattern_ty = Type::Named {
                                def,
                                args: match scrutinee {
                                    Type::Named { def: s, args } if *s == def => args.clone(),
                                    _ => vec![],
                                },
                            };
                            self.require_compatible(&pattern_ty, scrutinee, pattern.span);
                            return;
                        }
                        QualifiedLookup::Reported => return,
                        _ => {}
                    }
                }
                // `Rank.Gold` — enum and variant named explicitly.
                let (Some(enum_ident), Some(variant_ident)) =
                    (path.segments.first(), path.segments.get(1))
                else {
                    return;
                };
                let Some(def) = self.lookup_type_name(&enum_ident.name) else {
                    self.error(
                        DiagCode::UnknownType,
                        enum_ident.span,
                        format!("`{}` does not name a type", enum_ident.name),
                    );
                    return;
                };
                let pattern_ty = Type::Named {
                    def,
                    args: match scrutinee {
                        Type::Named { def: s, args } if *s == def => args.clone(),
                        _ => vec![],
                    },
                };
                self.require_compatible(&pattern_ty, scrutinee, pattern.span);
                if self.defs.variant_named(def, &variant_ident.name).is_none() {
                    let message = format!(
                        "`{}` has no variant named `{}`",
                        enum_ident.name, variant_ident.name
                    );
                    self.error(DiagCode::UnknownVariant, variant_ident.span, message);
                }
            }

            ast::PatternKind::Variant { path, elements } => {
                let (def, variant_name) = match path.segments.as_slice() {
                    [variant] => match scrutinee {
                        Type::Named { def, .. }
                            if self.defs.variant_named(*def, &variant.name).is_some() =>
                        {
                            (*def, variant.name.clone())
                        }
                        Type::Error | Type::Hole(_) => {
                            for element in elements {
                                self.bind_pattern(element, &Type::Error, mutable);
                            }
                            return;
                        }
                        _ => {
                            let message = format!(
                                "`{}` has no variant named `{}`",
                                self.render(scrutinee),
                                variant.name
                            );
                            self.error(DiagCode::UnknownVariant, variant.span, message);
                            for element in elements {
                                self.bind_pattern(element, &Type::Error, mutable);
                            }
                            return;
                        }
                    },
                    [enum_ident, variant_ident] => {
                        let Some(def) = self.lookup_type_name(&enum_ident.name) else {
                            self.error(
                                DiagCode::UnknownType,
                                enum_ident.span,
                                format!("`{}` does not name a type", enum_ident.name),
                            );
                            return;
                        };
                        (def, variant_ident.name.clone())
                    }
                    segments => {
                        // `game.player.Rank.Gold(payload)`.
                        let names: Vec<String> = segments.iter().map(|s| s.name.clone()).collect();
                        match self.qualified_ref(&names, pattern.span) {
                            QualifiedLookup::Variant(def, variant) => (def, variant),
                            QualifiedLookup::Reported => {
                                for element in elements {
                                    self.bind_pattern(element, &Type::Error, mutable);
                                }
                                return;
                            }
                            _ => return,
                        }
                    }
                };

                let Some(variant) = self.defs.variant_named(def, &variant_name) else {
                    let message = format!(
                        "`{}` has no variant named `{variant_name}`",
                        self.defs.name_of(def)
                    );
                    self.error(DiagCode::UnknownVariant, path.span, message);
                    return;
                };
                let payload = variant.payload.clone();

                // Instantiate payload types from the scrutinee's arguments.
                let bindings: Vec<(String, Type)> = match scrutinee {
                    Type::Named { def: s, args } if *s == def => self
                        .defs
                        .def(def)
                        .generics
                        .iter()
                        .cloned()
                        .zip(args.iter().cloned())
                        .collect(),
                    _ => {
                        let pattern_ty = Type::Named {
                            def,
                            args: vec![Type::Error; self.defs.def(def).generics.len()],
                        };
                        self.require_compatible(&pattern_ty, scrutinee, pattern.span);
                        Vec::new()
                    }
                };

                if elements.len() != payload.len() {
                    let message = format!(
                        "`{variant_name}` carries {} value(s), this pattern names {}",
                        payload.len(),
                        elements.len()
                    );
                    self.error(DiagCode::WrongArgumentCount, pattern.span, message);
                }
                for (element, payload_ty) in elements.iter().zip(payload.iter()) {
                    self.bind_pattern(element, &payload_ty.substitute(&bindings), mutable);
                }
            }

            ast::PatternKind::Struct { path, fields } => {
                let Some(first) = path.segments.first() else {
                    return;
                };
                let def = if path.segments.len() >= 2 && self.ctx.is_some() {
                    // `game.player.Player { .. }` in pattern position.
                    let names: Vec<String> = path.segments.iter().map(|s| s.name.clone()).collect();
                    match self.qualified_ref(&names, pattern.span) {
                        QualifiedLookup::Type(def) => def,
                        QualifiedLookup::Reported => return,
                        _ => {
                            self.error(
                                DiagCode::UnknownType,
                                pattern.span,
                                format!("`{}` does not name a type", names.join(".")),
                            );
                            return;
                        }
                    }
                } else if let Some(def) = self.lookup_type_name(&first.name) {
                    def
                } else {
                    self.error(
                        DiagCode::UnknownType,
                        first.span,
                        format!("`{}` does not name a type", first.name),
                    );
                    return;
                };
                let DefKind::Struct {
                    fields: declared_fields,
                } = &self.defs.def(def).kind
                else {
                    self.error(
                        DiagCode::UnknownType,
                        first.span,
                        format!("`{}` is not a struct", first.name),
                    );
                    return;
                };
                let declared: Vec<(String, Type)> = declared_fields
                    .iter()
                    .map(|f| (f.name.clone(), f.ty.clone()))
                    .collect();

                let bindings: Vec<(String, Type)> = match scrutinee {
                    Type::Named { def: s, args } if *s == def => self
                        .defs
                        .def(def)
                        .generics
                        .iter()
                        .cloned()
                        .zip(args.iter().cloned())
                        .collect(),
                    _ => Vec::new(),
                };

                for field in fields {
                    let Some((_, field_ty)) = declared.iter().find(|(n, _)| *n == field.name.name)
                    else {
                        let message =
                            format!("`{}` has no field named `{}`", first.name, field.name.name);
                        self.error(DiagCode::UnknownField, field.name.span, message);
                        continue;
                    };
                    let concrete = field_ty.substitute(&bindings);
                    match &field.pattern {
                        Some(sub) => self.bind_pattern(sub, &concrete, mutable),
                        None => self.bind(&field.name.name, concrete, mutable),
                    }
                }
            }

            ast::PatternKind::Or(alternatives) => {
                // Every alternative must bind the same names for the arm body
                // to be well-scoped; checked shallowly here.
                for alternative in alternatives {
                    self.bind_pattern(alternative, scrutinee, mutable);
                }
            }
        }
    }
}

/// Every name a pattern will bind, for the definite-initialization rule
/// (design/0014 §2): these are the names a closure inside the initializer
/// must not reach for.
pub(super) fn pattern_names(pattern: &ast::Pattern, out: &mut Vec<String>) {
    match &pattern.kind {
        ast::PatternKind::Binding(ident) => out.push(ident.name.clone()),
        ast::PatternKind::Variant { elements, .. } => {
            for element in elements {
                pattern_names(element, out);
            }
        }
        ast::PatternKind::Struct { fields, .. } => {
            for field in fields {
                match &field.pattern {
                    Some(pattern) => pattern_names(pattern, out),
                    None => out.push(field.name.name.clone()),
                }
            }
        }
        ast::PatternKind::Or(alternatives) => {
            for alternative in alternatives {
                pattern_names(alternative, out);
            }
        }
        ast::PatternKind::Wildcard
        | ast::PatternKind::Literal(_)
        | ast::PatternKind::Path(_)
        | ast::PatternKind::Error => {}
    }
}
