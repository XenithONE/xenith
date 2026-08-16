use std::sync::Arc;

use xenith_sema::def::DefKind;
use xenith_syntax::ast;

use super::{Env, Eval, Interp, Value, trap};

impl<'a> Interp<'a> {
    // ----- places (assignment targets) -----

    pub(super) fn read_place(
        &mut self,
        target: &'a ast::Expr,
        env: &mut Env<'a>,
    ) -> Eval<'a, Value<'a>> {
        self.eval(target, env)
    }

    pub(super) fn write_place(
        &mut self,
        target: &'a ast::Expr,
        value: Value<'a>,
        env: &mut Env<'a>,
    ) -> Eval<'a, ()> {
        let slot = self.resolve_place(target, env)?;
        *slot = value;
        Ok(())
    }

    /// Resolve an assignment target to the slot it names.
    ///
    /// The recursion is the copy-on-write contract (design/0017 §4): a
    /// binding in the environment is already unshared, and every aggregate
    /// node the path descends through is uniquified with `Arc::make_mut`
    /// *before* the descent continues. Uniquifying only the outer node and
    /// then writing through a shared inner one is precisely the bug the RFC
    /// names — the write would be visible through a value somebody else
    /// already read out (D1).
    pub(super) fn resolve_place<'e>(
        &self,
        target: &'a ast::Expr,
        env: &'e mut Env<'a>,
    ) -> Eval<'a, &'e mut Value<'a>> {
        match &target.kind {
            ast::ExprKind::Path(path) => {
                let name = &path.segments[0].name;
                match env.get_mut(name) {
                    Some(slot) => Ok(slot),
                    None => trap(target.span, format!("no binding named `{name}`")),
                }
            }
            ast::ExprKind::Field { receiver, name } => {
                let table = self.table;
                let base = self.resolve_place(receiver, env)?;
                let Value::Struct { def, fields } = base else {
                    return trap(target.span, "not a struct");
                };
                let DefKind::Struct {
                    fields: declared, ..
                } = &table.def(*def).kind
                else {
                    return trap(target.span, "not a struct");
                };
                match declared.iter().position(|f| f.name == name.name) {
                    Some(index) => Ok(&mut Arc::make_mut(fields)[index]),
                    None => trap(target.span, format!("no field `{}`", name.name)),
                }
            }
            _ => trap(target.span, "this expression cannot be assigned to"),
        }
    }
}
