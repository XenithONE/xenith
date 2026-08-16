use xenith_diag::{MAX_SIGNATURE_BYTES, MAX_TEACH_ITEMS, Teach, TeachItem};

use crate::def::MethodSig;
use crate::ty::{EffectSet, Type};

use super::Checker;

/// The most teaching blocks one check run may attach, first come first
/// served in diagnostic order (design/0009 §3: a total budget, so chained
/// failures cannot snowball into a wall of catalogues).
const MAX_TEACHES_PER_CHECK: usize = 5;

/// Teaching state shared across every function in the run — the budget
/// belongs to the check, not to one body.
pub(crate) struct TeachBudget {
    blocks_left: usize,
    /// Receiver types whose method catalogue has already been taught; one
    /// catalogue per type per run.
    catalogued: Vec<String>,
    /// `(receiver type, unknown member)` pairs whose module-call bridge has
    /// already been taught — the design/0012 §1 dedup key.
    module_called: Vec<(String, String)>,
}

impl TeachBudget {
    pub(crate) fn new() -> TeachBudget {
        TeachBudget {
            blocks_left: MAX_TEACHES_PER_CHECK,
            catalogued: Vec::new(),
            module_called: Vec::new(),
        }
    }
}

impl<'a> Checker<'a> {
    /// Claim budget for one teaching block. `catalogue_of` names the
    /// receiver type for a method catalogue, which appears at most once per
    /// run however many diagnostics ask for it (design/0009 §3).
    pub(super) fn claim_teach(&mut self, catalogue_of: Option<&str>) -> bool {
        if self.teach_budget.blocks_left == 0 {
            return false;
        }
        if let Some(type_name) = catalogue_of {
            if self.teach_budget.catalogued.iter().any(|t| t == type_name) {
                return false;
            }
            self.teach_budget.catalogued.push(type_name.to_string());
        }
        self.teach_budget.blocks_left -= 1;
        true
    }

    /// Claim budget for one module-call bridge. The dedup key is the pair
    /// `(receiver type, unknown member)` (design/0012 §1): the same wrong
    /// member on the same type teaches once per run, a different member
    /// earns its own bridge.
    pub(super) fn claim_module_call_teach(&mut self, type_name: &str, member: &str) -> bool {
        if self.teach_budget.blocks_left == 0 {
            return false;
        }
        let key = (type_name.to_string(), member.to_string());
        if self.teach_budget.module_called.contains(&key) {
            return false;
        }
        self.teach_budget.module_called.push(key);
        self.teach_budget.blocks_left -= 1;
        true
    }

    /// `name(param: Type, ..) -> Ret uses {..}` — the spelling the
    /// producers query uses, so a taught signature and a queried one agree.
    pub(super) fn signature_text(
        &self,
        name: &str,
        param_names: &[String],
        param_types: &[Type],
        ret: &Type,
        effects: &EffectSet,
    ) -> String {
        let params: Vec<String> = param_names
            .iter()
            .zip(param_types)
            .map(|(param, ty)| format!("{param}: {}", self.render(ty)))
            .collect();
        let mut text = format!("{name}({}) -> {}", params.join(", "), self.render(ret));
        if !effects.is_empty() {
            text.push_str(&format!(" uses {effects}"));
        }
        text
    }

    /// The receiver's methods with its generics substituted, in declaration
    /// order — `insert(key: String, value: Int) -> Option<Int>`, not the
    /// schematic form. `None` when there is nothing to teach.
    pub(super) fn method_catalogue(&self, receiver: &Type, methods: &[MethodSig]) -> Option<Teach> {
        if methods.is_empty() {
            return None;
        }
        let bindings: Vec<(String, Type)> = match receiver {
            Type::Named { def, args } => self
                .defs
                .def(*def)
                .generics
                .iter()
                .cloned()
                .zip(args.iter().cloned())
                .collect(),
            _ => Vec::new(),
        };
        let items = methods
            .iter()
            .map(|method| {
                let names: Vec<String> = method.params.iter().map(|(n, _)| n.to_string()).collect();
                let types: Vec<Type> = method
                    .params
                    .iter()
                    .map(|(_, t)| t.substitute(&bindings))
                    .collect();
                TeachItem::new(
                    method.name,
                    self.signature_text(
                        method.name,
                        &names,
                        &types,
                        &method.ret.substitute(&bindings),
                        &method.effects,
                    ),
                )
            })
            .collect();
        Some(Teach::available_methods(self.render(receiver), items))
    }

    /// The defining module of a nominal receiver type, when there is one —
    /// the gate for the module-call teach (design/0012 §1). Prelude types
    /// are bare-named and answer `None`; so do type variables and poison,
    /// which never reach here as `Type::Named`.
    pub(super) fn module_owner_of(&self, receiver: &Type) -> Option<String> {
        let Type::Named { def, .. } = receiver else {
            return None;
        };
        let name = self.defs.name_of(*def);
        name.rsplit_once('.').map(|(owner, _)| owner.to_string())
    }

    /// A function key spelled the way this module calls it: the current
    /// module's own items bare, everything else fully qualified. Unlike
    /// `display_fn` this strips only an exact module match, so a sibling
    /// nested module never renders half-qualified.
    pub(super) fn call_spelling(&self, owner: &str, bare: &str) -> String {
        match self.ctx {
            Some(ctx) if ctx.prefix == owner => bare.to_string(),
            _ => format!("{owner}.{bare}"),
        }
    }

    /// The module-call bridge for an unknown member on a module-owned type
    /// (design/0012 §1): the defining module's `pub` functions whose input
    /// parameters take the receiver type directly. Return-only matches are
    /// excluded — the producers lesson (design/0009 §7) carried through.
    ///
    /// Ranking is deterministic: a function whose name equals the unknown
    /// member comes first (and therefore always fits the unchanged budget —
    /// the displacement invariant), then first-parameter matches, then other
    /// input positions, ties broken by fully-qualified name. Candidates are
    /// included or omitted whole: a signature over the byte budget drops the
    /// candidate rather than cutting the signature, and `total_items` keeps
    /// the omission structural.
    pub(super) fn module_call_teach(
        &self,
        receiver: &Type,
        owner: &str,
        member: &str,
    ) -> Option<Teach> {
        let Type::Named {
            def: receiver_def, ..
        } = receiver
        else {
            return None;
        };
        // (tier, fully-qualified name, fn index, fitting input positions).
        let mut ranked: Vec<(u8, String, usize, Vec<usize>)> = Vec::new();
        for (index, sig) in self.defs.fns.iter().enumerate() {
            let Some((fn_owner, bare)) = sig.name.rsplit_once('.') else {
                continue;
            };
            if fn_owner != owner || !sig.is_pub {
                continue;
            }
            let fitting: Vec<usize> = sig
                .params
                .iter()
                .enumerate()
                .filter(|(_, (_, ty))| matches!(ty, Type::Named { def, .. } if def == receiver_def))
                .map(|(position, _)| position)
                .collect();
            if fitting.is_empty() {
                continue; // return-only or unrelated: no bridge to offer.
            }
            let tier = if bare == member {
                0
            } else if fitting[0] == 0 {
                1
            } else {
                2
            };
            ranked.push((tier, sig.name.clone(), index, fitting));
        }
        if ranked.is_empty() {
            return None;
        }
        ranked.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

        let total_items = ranked.len();
        let mut items = Vec::new();
        for (_, key, index, fitting) in &ranked {
            if items.len() == MAX_TEACH_ITEMS {
                break;
            }
            let sig = &self.defs.fns[*index];
            let bare = key.rsplit_once('.').map(|(_, b)| b).unwrap_or(key);
            let spelled = self.call_spelling(owner, bare);
            let names: Vec<String> = sig.params.iter().map(|(n, _)| n.clone()).collect();
            let types: Vec<Type> = sig.params.iter().map(|(_, t)| t.clone()).collect();
            let text = self.signature_text(&spelled, &names, &types, &sig.ret, &sig.effects);
            if text.len() > MAX_SIGNATURE_BYTES {
                continue; // omit the whole candidate, never cut its signature.
            }
            let mut item = TeachItem::new(key.clone(), text);
            if let [only] = fitting.as_slice() {
                // Exactly one input position fits, so the rewrite is honest.
                // Several fitting positions stay signature-only: guessing
                // which one takes the receiver would be mis-guidance.
                item.receiver_parameter = Some(names[*only].clone());
                let args: Vec<String> = names
                    .iter()
                    .enumerate()
                    .map(|(position, name)| {
                        if position == *only {
                            format!("{name}: <receiver>")
                        } else {
                            format!("{name}: ...")
                        }
                    })
                    .collect();
                item.rewrite = Some(format!("{spelled}({})", args.join(", ")));
            }
            items.push(item);
        }
        if items.is_empty() {
            // Every candidate was over the byte budget: an empty block
            // teaches nothing and claims none (the 0009 catalogue rule).
            return None;
        }
        Some(Teach::module_call(
            self.render(receiver),
            items,
            total_items,
        ))
    }
}

/// The unique nearest name within two edits of `written`, or nothing.
///
/// A tie is silence: suggesting one of two equally close names is a coin
/// toss presented as knowledge (design/0009 §3, the cursor minimal form).
/// Duplicate candidates collapse first so a name shadowed in two scopes
/// cannot tie with itself.
pub(super) fn did_you_mean<I>(written: &str, candidates: I) -> Option<String>
where
    I: IntoIterator<Item = String>,
{
    let mut seen: Vec<String> = Vec::new();
    let mut best: Option<(usize, String)> = None;
    let mut tied = false;
    for candidate in candidates {
        if seen.contains(&candidate) {
            continue;
        }
        seen.push(candidate.clone());
        let distance = edit_distance(written, &candidate);
        match &best {
            Some((held, _)) if distance > *held => {}
            Some((held, _)) if distance == *held => tied = true,
            _ => {
                best = Some((distance, candidate));
                tied = false;
            }
        }
    }
    match best {
        Some((distance, name)) if distance <= 2 && !tied => Some(name),
        _ => None,
    }
}

/// Restricted Damerau-Levenshtein (optimal string alignment): insert,
/// delete, substitute, and adjacent transposition each cost one. Counted in
/// characters, case not folded — the naming rules make case meaningful.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    // Three rolling rows; the row before last is what prices transposition.
    let mut before_prev: Vec<usize> = vec![0; b.len() + 1];
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut current: Vec<usize> = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        current[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            current[j] = (prev[j] + 1)
                .min(current[j - 1] + 1)
                .min(prev[j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                current[j] = current[j].min(before_prev[j - 2] + 1);
            }
        }
        std::mem::swap(&mut before_prev, &mut prev);
        std::mem::swap(&mut prev, &mut current);
    }
    prev[b.len()]
}
