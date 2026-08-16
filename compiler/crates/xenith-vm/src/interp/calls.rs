use std::sync::Arc;

use xenith_diag::Span;
use xenith_sema::def::DefKind;
use xenith_syntax::ast;

use super::eval::expr_segments;
use super::value::{compare, values_equal};
use super::{
    Body, Control, Env, Eval, Interp, RuntimeRef, Value, err_index, find_fn, none_index, ok_index,
    some_index, trap,
};

impl<'a> Interp<'a> {
    // ----- calls -----

    pub(super) fn call(
        &mut self,
        callee: &'a ast::Expr,
        args: &'a [ast::Arg],
        span: Span,
        env: &mut Env<'a>,
    ) -> Eval<'a, Value<'a>> {
        // Named functions and variant constructors resolve before locals do
        // not shadow them — same order as the checker.
        let callee_value = match &callee.kind {
            ast::ExprKind::Path(path) if path.segments.len() == 1 => {
                // The one prelude free function (design/0007 D4). A user
                // declaration of the same name is a duplicate-definition
                // error, so nothing real is shadowed here.
                let name = &path.segments[0].name;
                if name == "empty_map"
                    && env.get(name).is_none()
                    && find_fn(self.current_module(), name).is_none()
                {
                    for arg in args {
                        self.eval(&arg.value, env)?;
                    }
                    return Ok(Value::map(Vec::new()));
                }
                self.path_value(path, callee.span, env)?
            }
            ast::ExprKind::Field { receiver, name } => {
                if let ast::ExprKind::Path(path) = &receiver.kind {
                    if let [single] = path.segments.as_slice() {
                        if env.get(&single.name).is_none() {
                            if let Some(def) = self.lookup_type(&single.name) {
                                self.variant_ref(def, &name.name, callee.span)?
                            } else {
                                self.eval(callee, env)?
                            }
                        } else {
                            self.eval(callee, env)?
                        }
                    } else {
                        self.eval(callee, env)?
                    }
                } else {
                    self.eval(callee, env)?
                }
            }
            _ => self.eval(callee, env)?,
        };

        let mut evaluated = Vec::with_capacity(args.len());
        for arg in args {
            evaluated.push(self.eval(&arg.value, env)?);
        }

        self.apply(callee_value, evaluated, span)
    }

    pub(super) fn apply(
        &mut self,
        callee: Value<'a>,
        args: Vec<Value<'a>>,
        span: Span,
    ) -> Eval<'a, Value<'a>> {
        self.safe_point()?;
        match callee {
            Value::Fn {
                params,
                body,
                captured,
                is_async,
                home,
            } => {
                if params.len() != args.len() {
                    return trap(span, "wrong number of arguments");
                }
                let mut env = Env::new();
                for (name, value) in captured.iter() {
                    env.bind(name, value.clone());
                }
                env.scopes.push(Vec::new());
                for (param, value) in params.iter().zip(args) {
                    env.bind(param, value);
                }
                // The callee's bare names live in its own module.
                let caller = self.current;
                self.current = home;
                let result = match body {
                    Body::Block(block) => self.block_inner(block, &mut env),
                    Body::Expr(expr) => self.eval(expr, &mut env),
                };
                self.current = caller;
                let value = match result {
                    Ok(value) | Err(Control::Return(value)) => value,
                    Err(other) => return Err(other),
                };
                if is_async {
                    Ok(Value::Task(Arc::new(value)))
                } else {
                    Ok(value)
                }
            }
            Value::VariantCtor {
                def,
                variant,
                arity,
            } => {
                if args.len() != arity {
                    return trap(span, "wrong number of constructor arguments");
                }
                Ok(Value::enumeration(def, variant, args))
            }
            _ => trap(span, "this value is not callable"),
        }
    }

    /// Built-in methods — the runtime half of the provisional prelude in
    /// `def.rs`. The two tables must agree; the examples exercise both.
    pub(super) fn method_call(
        &mut self,
        receiver: &'a ast::Expr,
        method: &'a ast::Ident,
        args: &'a [ast::Arg],
        span: Span,
        env: &mut Env<'a>,
    ) -> Eval<'a, Value<'a>> {
        // `Grade.Pass(95)` parses as a method call; construct the variant.
        // Mirrors the checker's resolution order exactly.
        if let ast::ExprKind::Path(path) = &receiver.kind {
            if let [single] = path.segments.as_slice() {
                if env.get(&single.name).is_none() {
                    if let Some(def) = self.lookup_type(&single.name) {
                        if self.table.variant_named(def, &method.name).is_some() {
                            let ctor = self.variant_ref(def, &method.name, span)?;
                            let mut evaluated = Vec::with_capacity(args.len());
                            for arg in args {
                                evaluated.push(self.eval(&arg.value, env)?);
                            }
                            return self.apply(ctor, evaluated, span);
                        }
                    }
                }
            }
        }

        // `game.scores.best(..)` / `game.player.Rank.Gold(..)` — resolved
        // against the module set before anything else is evaluated.
        if let Some(receiver_segments) = expr_segments(receiver) {
            if env.get(&receiver_segments[0]).is_none() {
                let mut segments = receiver_segments;
                segments.push(method.name.clone());
                if let Some(reference) = self.runtime_ref(&segments) {
                    let mut evaluated = Vec::with_capacity(args.len());
                    for arg in args {
                        evaluated.push(self.eval(&arg.value, env)?);
                    }
                    return match reference {
                        RuntimeRef::Fn(home, bare) => {
                            let callee = self.fn_value(home, &bare, span)?;
                            self.apply(callee, evaluated, span)
                        }
                        // A const is not callable; the checker refused it.
                        RuntimeRef::Const(_, bare) => {
                            trap(span, format!("`{bare}` is a const, not a fn"))
                        }
                        RuntimeRef::Variant(def, variant) => {
                            let ctor = self.variant_ref(def, &variant, span)?;
                            self.apply(ctor, evaluated, span)
                        }
                    };
                }
            }
        }

        // The container mutators write through the receiver in place, so it
        // is resolved as a place — the same resolution `=` uses — rather than
        // evaluated to a copy. Arguments go first, as assignment evaluates
        // its right-hand side first, so the place borrow overlaps nothing.
        if matches!(
            method.name.as_str(),
            "push" | "pop" | "replace" | "insert" | "remove"
        ) {
            let mut evaluated = Vec::with_capacity(args.len());
            for arg in args {
                evaluated.push(self.eval(&arg.value, env)?);
            }
            let slot = self.resolve_place(receiver, env)?;
            // `resolve_place` already uniquified every node on the way here
            // (design/0017 §4); `make_mut` finishes the job at the leaf. A
            // shared node is copied before it is written, so a value read out
            // of this container earlier stays exactly as it was (D1).
            return match (&mut *slot, method.name.as_str()) {
                (Value::List(items), "push") => {
                    let Some(item) = evaluated.into_iter().next() else {
                        return trap(span, "push takes a value");
                    };
                    Arc::make_mut(items).push(item);
                    Ok(Value::Unit)
                }
                (Value::List(items), "pop") => {
                    let popped = Arc::make_mut(items).pop();
                    Ok(self.option_of(popped))
                }
                (Value::List(items), "replace") => {
                    let mut taken = evaluated.into_iter();
                    let (Some(Value::Int(index)), Some(value)) = (taken.next(), taken.next())
                    else {
                        return trap(span, "replace takes an index and a value");
                    };
                    // Out of range leaves the list untouched (0007 §3) — and
                    // must not copy it either, so the bounds test comes first.
                    let target = usize::try_from(index).ok().filter(|i| *i < items.len());
                    let old =
                        target.map(|i| std::mem::replace(&mut Arc::make_mut(items)[i], value));
                    Ok(self.option_of(old))
                }
                (Value::Map(entries), "insert") => {
                    let mut taken = evaluated.into_iter();
                    let (Some(key), Some(value)) = (taken.next(), taken.next()) else {
                        return trap(span, "insert takes a key and a value");
                    };
                    // An existing key keeps its position and its stored key;
                    // only the value moves (0007 §3 normative order).
                    let mut existing = None;
                    for (index, (stored, _)) in entries.iter().enumerate() {
                        if values_equal(stored, &key, span)? {
                            existing = Some(index);
                            break;
                        }
                    }
                    match existing {
                        Some(index) => {
                            let old =
                                std::mem::replace(&mut Arc::make_mut(entries)[index].1, value);
                            Ok(self.option_of(Some(old)))
                        }
                        None => {
                            Arc::make_mut(entries).push((key, value));
                            Ok(self.option_of(None))
                        }
                    }
                }
                (Value::Map(entries), "remove") => {
                    let Some(key) = evaluated.into_iter().next() else {
                        return trap(span, "remove takes a key");
                    };
                    let mut found = None;
                    for (index, (stored, _)) in entries.iter().enumerate() {
                        if values_equal(stored, &key, span)? {
                            found = Some(index);
                            break;
                        }
                    }
                    // Vec::remove shifts, so the survivors keep their order;
                    // a later re-insert of the key lands at the end. A miss
                    // writes nothing, so it does not copy either.
                    let removed = found.map(|index| Arc::make_mut(entries).remove(index).1);
                    Ok(self.option_of(removed))
                }
                _ => trap(
                    span,
                    format!("no runtime method `{}` for this value", method.name),
                ),
            };
        }

        let receiver_value = self.eval(receiver, env)?;
        let mut evaluated = Vec::with_capacity(args.len());
        for arg in args {
            evaluated.push(self.eval(&arg.value, env)?);
        }

        match (&receiver_value, method.name.as_str()) {
            (Value::Int(a), "checked_add") => {
                let Some(Value::Int(b)) = evaluated.first() else {
                    return trap(span, "checked_add takes an Int");
                };
                Ok(match a.checked_add(*b) {
                    Some(sum) => {
                        Value::enumeration(self.table.option, some_index(), vec![Value::Int(sum)])
                    }
                    None => Value::enumeration(self.table.option, none_index(), Vec::new()),
                })
            }
            (Value::Int(a), "to_text") => Ok(Value::str(a.to_string())),
            (Value::Str(a), "concat") => {
                let Some(Value::Str(b)) = evaluated.first() else {
                    return trap(span, "concat takes a String");
                };
                Ok(Value::str(format!("{a}{b}")))
            }
            // `len` counts Unicode scalar values, never bytes (D2).
            (Value::Str(a), "len") => Ok(Value::Int(a.chars().count() as i64)),
            (Value::Str(a), "split") => {
                let Some(Value::Str(sep)) = evaluated.first() else {
                    return trap(span, "split takes a String");
                };
                // Lossless by construction: `pieces.join(sep)` rebuilds the
                // input exactly, empty pieces included. The empty separator
                // is the `chars` replacement — one piece per scalar.
                let pieces: Vec<Value> = if sep.is_empty() {
                    a.chars().map(|c| Value::str(c.to_string())).collect()
                } else {
                    a.split(sep.as_str())
                        .map(|piece| Value::str(piece.to_string()))
                        .collect()
                };
                Ok(Value::list(pieces))
            }
            (Value::Str(a), "trim") => Ok(Value::str(
                a.trim_matches(|c: char| matches!(c, ' ' | '\t' | '\r' | '\n'))
                    .to_string(),
            )),
            (Value::Str(a), "try_to_int") => {
                // Accepted shape: ASCII whitespace, then [+-]?[0-9]+ (0007
                // §3). Everything else — separators, decimals, overflow — is
                // an Err value, never a trap.
                let trimmed = a.trim_matches(|c: char| matches!(c, ' ' | '\t' | '\r' | '\n'));
                Ok(match trimmed.parse::<i64>() {
                    Ok(value) => {
                        Value::enumeration(self.table.result, ok_index(), vec![Value::Int(value)])
                    }
                    Err(error) => {
                        let message = match error.kind() {
                            std::num::IntErrorKind::PosOverflow
                            | std::num::IntErrorKind::NegOverflow => "out of Int range",
                            _ => "not an integer",
                        };
                        Value::enumeration(
                            self.table.result,
                            err_index(),
                            vec![Value::error_value(message)],
                        )
                    }
                })
            }
            (Value::Str(a), "starts_with") => {
                let Some(Value::Str(prefix)) = evaluated.first() else {
                    return trap(span, "starts_with takes a String");
                };
                Ok(Value::Bool(a.starts_with(prefix.as_str())))
            }
            (Value::Str(a), "contains") => {
                let Some(Value::Str(sub)) = evaluated.first() else {
                    return trap(span, "contains takes a String");
                };
                Ok(Value::Bool(a.contains(sub.as_str())))
            }
            (Value::List(items), "len") => Ok(Value::Int(items.len() as i64)),
            (Value::List(items), "is_empty") => Ok(Value::Bool(items.is_empty())),
            // ----- the design/0014 §4 combinators -----
            //
            // The only way a closure is ever invoked: by std, left to right,
            // returning new values. The receiver is a copy and stays whole.
            (Value::List(items), "map") => {
                let Some(f) = evaluated.into_iter().next() else {
                    return trap(span, "map takes a closure");
                };
                let mut out = Vec::with_capacity(items.len());
                for item in items.iter() {
                    out.push(self.apply(f.clone(), vec![item.clone()], span)?);
                }
                Ok(Value::list(out))
            }
            (Value::List(items), "filter") => {
                let Some(f) = evaluated.into_iter().next() else {
                    return trap(span, "filter takes a closure");
                };
                let mut out = Vec::new();
                for item in items.iter() {
                    match self.apply(f.clone(), vec![item.clone()], span)? {
                        Value::Bool(true) => out.push(item.clone()),
                        Value::Bool(false) => {}
                        _ => return trap(span, "`filter` needs its closure to return a Bool"),
                    }
                }
                Ok(Value::list(out))
            }
            (Value::List(items), "fold") => {
                // Left fold: `fold(init: 0, f: |acc, x| ..)` — the named
                // argument rule fixed the order at the call site.
                let mut taken = evaluated.into_iter();
                let (Some(init), Some(f)) = (taken.next(), taken.next()) else {
                    return trap(span, "fold takes an initial value and a closure");
                };
                let mut acc = init;
                for item in items.iter() {
                    acc = self.apply(f.clone(), vec![acc, item.clone()], span)?;
                }
                Ok(acc)
            }
            (Value::List(items), "find") => {
                // Short-circuits: elements after the first hit are never
                // touched (design/0014 §4 — the contract, not an optimisation).
                let Some(f) = evaluated.into_iter().next() else {
                    return trap(span, "find takes a closure");
                };
                for item in items.iter() {
                    match self.apply(f.clone(), vec![item.clone()], span)? {
                        Value::Bool(true) => return Ok(self.option_of(Some(item.clone()))),
                        Value::Bool(false) => {}
                        _ => return trap(span, "`find` needs its closure to return a Bool"),
                    }
                }
                Ok(self.option_of(None))
            }
            (Value::List(items), "get") => {
                let Some(Value::Int(index)) = evaluated.first() else {
                    return trap(span, "get takes an Int");
                };
                // Negative and out-of-range are both None; the hit is a copy
                // of the element (D1).
                let item = usize::try_from(*index)
                    .ok()
                    .and_then(|i| items.get(i))
                    .cloned();
                Ok(self.option_of(item))
            }
            (Value::List(items), "contains") => {
                let Some(needle) = evaluated.first() else {
                    return trap(span, "contains takes a value");
                };
                let mut found = false;
                for item in items.iter() {
                    if values_equal(item, needle, span)? {
                        found = true;
                        break;
                    }
                }
                Ok(Value::Bool(found))
            }
            (Value::List(items), "sorted") => {
                // Insertion keeps the sort stable and lets a comparison trap
                // propagate, which `sort_by` cannot.
                let mut sorted = items.as_ref().clone();
                let mut i = 1;
                while i < sorted.len() {
                    let mut j = i;
                    while j > 0 {
                        let ordering = compare(&sorted[j - 1], &sorted[j], span)?;
                        if ordering != Some(std::cmp::Ordering::Greater) {
                            break;
                        }
                        sorted.swap(j - 1, j);
                        j -= 1;
                    }
                    i += 1;
                }
                Ok(Value::list(sorted))
            }
            (Value::List(items), "concat") => {
                let Some(Value::List(other)) = evaluated.first() else {
                    return trap(span, "concat takes a List");
                };
                let mut joined = items.as_ref().clone();
                joined.extend(other.iter().cloned());
                Ok(Value::list(joined))
            }
            (Value::List(items), "join") => {
                let Some(Value::Str(sep)) = evaluated.first() else {
                    return trap(span, "join takes a String");
                };
                let rendered: Vec<String> =
                    items.iter().map(|item| self.value_text(item)).collect();
                Ok(Value::str(rendered.join(sep.as_str())))
            }
            (Value::Map(entries), "len") => Ok(Value::Int(entries.len() as i64)),
            (Value::Map(entries), "is_empty") => Ok(Value::Bool(entries.is_empty())),
            (Value::Map(entries), "get") => {
                let Some(key) = evaluated.first() else {
                    return trap(span, "get takes a key");
                };
                let mut hit = None;
                for (stored, value) in entries.iter() {
                    if values_equal(stored, key, span)? {
                        // D1: the read is a copy of the value.
                        hit = Some(value.clone());
                        break;
                    }
                }
                Ok(self.option_of(hit))
            }
            (Value::Map(entries), "has_key") => {
                let Some(key) = evaluated.first() else {
                    return trap(span, "has_key takes a key");
                };
                let mut found = false;
                for (stored, _) in entries.iter() {
                    if values_equal(stored, key, span)? {
                        found = true;
                        break;
                    }
                }
                Ok(Value::Bool(found))
            }
            // Insertion-order snapshot: later mutation of the map must not
            // reach into a list already handed out (0007 §3).
            (Value::Map(entries), "keys") => Ok(Value::list(
                entries.iter().map(|(key, _)| key.clone()).collect(),
            )),
            (Value::Enum { def, variant, .. }, "to_result") if *def == self.table.option => {
                let error = evaluated.into_iter().next().unwrap_or(Value::Unit);
                let Value::Enum {
                    variant, payload, ..
                } = receiver_value
                else {
                    unreachable!("matched above");
                };
                Ok(if variant == some_index() {
                    Value::Enum {
                        def: self.table.result,
                        variant: ok_index(),
                        payload,
                    }
                } else {
                    Value::enumeration(self.table.result, err_index(), vec![error])
                })
            }
            (Value::Capability("Io"), "write") => {
                let Some(Value::Str(text)) = evaluated.first() else {
                    return trap(span, "write takes a String");
                };
                self.stdout.extend_from_slice(text.as_bytes());
                Ok(Value::enumeration(
                    self.table.result,
                    ok_index(),
                    vec![Value::Unit],
                ))
            }
            _ => trap(
                span,
                format!("no runtime method `{}` for this value", method.name),
            ),
        }
    }

    /// `Some(value)` / `None` from a Rust `Option`.
    fn option_of(&self, value: Option<Value<'a>>) -> Value<'a> {
        match value {
            Some(value) => Value::enumeration(self.table.option, some_index(), vec![value]),
            None => Value::enumeration(self.table.option, none_index(), Vec::new()),
        }
    }

    /// Total, deterministic rendering — the runtime face of the sealed `Text`
    /// property, which is total today (design/0006 §3-5). `String` renders
    /// verbatim; everything else the way a literal would be written.
    fn value_text(&self, value: &Value<'a>) -> String {
        match value {
            Value::Int(v) => v.to_string(),
            Value::Float(v) => v.to_string(),
            Value::Bool(v) => v.to_string(),
            Value::Str(v) => v.as_ref().clone(),
            Value::Char(v) => v.to_string(),
            Value::Unit => "unit".to_string(),
            Value::List(items) => {
                let parts: Vec<String> = items.iter().map(|item| self.value_text(item)).collect();
                format!("[{}]", parts.join(", "))
            }
            // Rendered in insertion order — deterministic by the normative
            // order rules, even though `==` ignores it.
            Value::Map(entries) => {
                let parts: Vec<String> = entries
                    .iter()
                    .map(|(key, value)| {
                        format!("{}: {}", self.value_text(key), self.value_text(value))
                    })
                    .collect();
                format!("{{{}}}", parts.join(", "))
            }
            Value::ErrorValue(message) => format!("Error({message})"),
            Value::Struct { def, fields } => {
                let name = self.table.name_of(*def);
                let DefKind::Struct { fields: declared } = &self.table.def(*def).kind else {
                    return name;
                };
                let parts: Vec<String> = declared
                    .iter()
                    .zip(fields.iter())
                    .map(|(field, value)| format!("{}: {}", field.name, self.value_text(value)))
                    .collect();
                format!("{name} {{ {} }}", parts.join(", "))
            }
            Value::Enum {
                def,
                variant,
                payload,
            } => {
                let name = match &self.table.def(*def).kind {
                    DefKind::Enum { variants } => variants
                        .get(*variant)
                        .map(|v| v.name.clone())
                        .unwrap_or_default(),
                    _ => String::new(),
                };
                if payload.is_empty() {
                    name
                } else {
                    let parts: Vec<String> =
                        payload.iter().map(|part| self.value_text(part)).collect();
                    format!("{name}({})", parts.join(", "))
                }
            }
            Value::Fn { .. } | Value::VariantCtor { .. } => "<fn>".to_string(),
            Value::Capability(name) => format!("<{name}>"),
            Value::Task(_) | Value::Pending { .. } => "<task>".to_string(),
        }
    }
}
