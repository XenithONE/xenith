//! The canonical formatter.
//!
//! There are no options. A configurable formatter cannot deliver the property
//! this one exists for: **the same meaning always produces the same bytes**.
//! That is what removes variance from generated code and noise from diffs.
//!
//! The formatter checks its own output before returning it. Formatting must not
//! change meaning, and a formatter that might silently do so is worse than
//! none — so if the rewritten source does not parse to the same tree, or if a
//! comment went missing, [`format`] returns an error instead of the output.
//!
//! Rules are recorded in `design/0005-canonical-formatting.md`.

use crate::ast::*;
use crate::lexer::lex;
use crate::parser::parse;
use crate::token::TokenKind;
use xenith_diag::{Diagnostic, Span};

/// Maximum line length before a construct switches to its multi-line form.
const WIDTH: usize = 100;
const INDENT: &str = "    ";

#[derive(Clone, Debug)]
pub enum FormatError {
    /// The input does not parse. Formatting something the compiler cannot read
    /// would be guessing.
    Unparsable(Vec<Diagnostic>),
    /// The formatter's own output does not parse. Always a formatter bug.
    OutputUnparsable(Vec<Diagnostic>),
    /// The output parses, but to a different tree. Always a formatter bug, and
    /// the one that would silently corrupt a user's program.
    MeaningChanged,
    /// A comment present in the input is absent from the output.
    CommentLost(String),
}

impl std::fmt::Display for FormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FormatError::Unparsable(diagnostics) => write!(
                f,
                "cannot format source with {} parse error(s)",
                diagnostics.len()
            ),
            FormatError::OutputUnparsable(_) => {
                f.write_str("formatter bug: the formatted output does not parse")
            }
            FormatError::MeaningChanged => {
                f.write_str("formatter bug: formatting would change the meaning of this program")
            }
            FormatError::CommentLost(text) => {
                write!(f, "formatter bug: the comment `{text}` would be lost")
            }
        }
    }
}

impl std::error::Error for FormatError {}

pub fn format(source: &str) -> Result<String, FormatError> {
    let parsed = parse(source);
    if !parsed.diagnostics.is_empty() {
        return Err(FormatError::Unparsable(parsed.diagnostics));
    }

    let output = render_module(source, &parsed.module);

    // --- self-check -------------------------------------------------------
    //
    // Everything below exists so that a bug here refuses to emit rather than
    // quietly rewriting someone's program into a different one.

    let reparsed = parse(&output);
    if !reparsed.diagnostics.is_empty() {
        return Err(FormatError::OutputUnparsable(reparsed.diagnostics));
    }

    let mut before = parsed.module;
    let mut after = reparsed.module;
    normalize_spans(&mut before);
    normalize_spans(&mut after);
    if before != after {
        return Err(FormatError::MeaningChanged);
    }

    if let Some(lost) = missing_comment(source, &output) {
        return Err(FormatError::CommentLost(lost));
    }

    Ok(output)
}

/// A comment text present in `source` but not in `output`, if any.
fn missing_comment(source: &str, output: &str) -> Option<String> {
    let mut after: Vec<String> = comment_texts(output);
    for text in comment_texts(source) {
        match after.iter().position(|t| *t == text) {
            Some(index) => {
                after.swap_remove(index);
            }
            None => return Some(text),
        }
    }
    None
}

fn comment_texts(source: &str) -> Vec<String> {
    lex(source)
        .tokens
        .iter()
        .filter(|t| matches!(t.kind, TokenKind::LineComment | TokenKind::DocComment))
        .filter_map(|t| t.span.slice(source))
        .map(|t| t.trim_end().to_string())
        .collect()
}

// ------------------------------------------------------------------- writer

/// A comment, and whether it documents the following declaration (`///`) or is
/// an ordinary note (`//`). The distinction matters when placing a file header.
#[derive(Clone, Copy)]
struct Comment {
    span: Span,
    is_doc: bool,
}

struct Writer<'a> {
    source: &'a str,
    /// Comments in source order, with the index of the first not yet written.
    /// Each is placed before the construct it precedes.
    comments: Vec<Comment>,
    next_comment: usize,
    out: String,
    depth: usize,
}

fn render_module(source: &str, module: &Module) -> String {
    let comments = lex(source)
        .tokens
        .iter()
        .filter_map(|t| match t.kind {
            TokenKind::LineComment => Some(Comment {
                span: t.span,
                is_doc: false,
            }),
            TokenKind::DocComment => Some(Comment {
                span: t.span,
                is_doc: true,
            }),
            _ => None,
        })
        .collect();

    let mut writer = Writer {
        source,
        comments,
        next_comment: 0,
        out: String::new(),
        depth: 0,
    };

    for (index, item) in module.items.iter().enumerate() {
        if index > 0 {
            writer.out.push('\n');
            writer.comments_before(item.span.start);
        } else {
            writer.file_header(item.span.start);
        }
        writer.item(item);
    }

    // Any comment after the last declaration still belongs in the file.
    writer.comments_before(u32::MAX);

    if writer.out.is_empty() {
        return String::new();
    }
    if !writer.out.ends_with('\n') {
        writer.out.push('\n');
    }
    writer.out
}

impl<'a> Writer<'a> {
    fn line(&mut self, text: &str) {
        for _ in 0..self.depth {
            self.out.push_str(INDENT);
        }
        self.out.push_str(text);
        self.out.push('\n');
    }

    fn width_left(&self) -> usize {
        WIDTH.saturating_sub(self.depth * INDENT.len())
    }

    /// Whether a block has nothing at all in it, comments included.
    ///
    /// An empty block collapses to `{}`. A block that looks empty but holds a
    /// comment must not, or the comment would have nowhere to go and the
    /// self-check would reject the whole file.
    fn block_is_empty(&self, block: &Block) -> bool {
        block.stmts.is_empty()
            && block.tail.is_none()
            && !self.comments[self.next_comment..]
                .iter()
                .any(|c| c.span.start >= block.span.start && c.span.end <= block.span.end)
    }

    /// Emit every comment that begins before `offset`.
    fn comments_before(&mut self, offset: u32) {
        while self.next_comment < self.comments.len() {
            let comment = self.comments[self.next_comment];
            if comment.span.start >= offset {
                break;
            }
            self.next_comment += 1;
            let text = comment
                .span
                .slice(self.source)
                .unwrap_or("")
                .trim_end()
                .to_string();
            self.line(&text);
        }
    }

    /// Comments before the first declaration, treating a leading run of `//`
    /// lines as a file header and separating it with a blank line.
    ///
    /// Without this the header runs straight into the first declaration — and
    /// where that declaration is documented, into its `///` lines, which reads
    /// as one confused block. The rule keys off position in the file rather
    /// than the input's blank lines, so it stays layout-independent.
    fn file_header(&mut self, offset: u32) {
        let mut in_header = false;
        while self.next_comment < self.comments.len() {
            let comment = self.comments[self.next_comment];
            if comment.span.start >= offset {
                break;
            }
            if comment.is_doc && in_header {
                // The header ends here; `in_header` is reset below by the
                // assignment that classifies this comment.
                self.out.push('\n');
            }
            self.next_comment += 1;
            let text = comment
                .span
                .slice(self.source)
                .unwrap_or("")
                .trim_end()
                .to_string();
            self.line(&text);
            in_header = !comment.is_doc;
        }
        if in_header {
            self.out.push('\n');
        }
    }

    // ----- items -----

    fn item(&mut self, item: &Item) {
        match &item.kind {
            ItemKind::Use(u) => {
                let path = render_path(&u.path);
                self.line(&format!("use {path};"));
            }
            ItemKind::Const(c) => {
                let prefix = if c.is_pub { "pub " } else { "" };
                let head = format!(
                    "{prefix}const {}: {} = {};",
                    c.name.name,
                    render_type(&c.ty),
                    render_expr(&c.value, PREC_LOWEST)
                );
                self.line(&head);
            }
            ItemKind::Fn(f) => self.fn_item(f),
            ItemKind::Struct(s) => self.struct_item(s),
            ItemKind::Enum(e) => self.enum_item(e),
            ItemKind::Error => {}
        }
    }

    fn fn_item(&mut self, f: &FnItem) {
        let mut head = String::new();
        if f.is_pub {
            head.push_str("pub ");
        }
        if f.is_async {
            head.push_str("async ");
        }
        head.push_str("fn ");
        head.push_str(&f.name.name);
        head.push_str(&render_generics(&f.generics));
        head.push('(');
        head.push_str(
            &f.params
                .iter()
                .map(|p| format!("{}: {}", p.name.name, render_type(&p.ty)))
                .collect::<Vec<_>>()
                .join(", "),
        );
        head.push(')');
        if let Some(ret) = &f.return_type {
            head.push_str(" -> ");
            head.push_str(&render_type(ret));
        }
        if let Some(effects) = &f.effects {
            head.push(' ');
            head.push_str(&render_effects(effects));
        }

        match &f.body {
            Some(body) if self.block_is_empty(body) => {
                head.push_str(" {}");
                self.line(&head);
            }
            Some(body) => {
                head.push_str(" {");
                self.line(&head);
                self.depth += 1;
                self.block_body(body);
                self.depth -= 1;
                self.line("}");
            }
            None => {
                head.push(';');
                self.line(&head);
            }
        }
    }

    fn struct_item(&mut self, s: &StructItem) {
        let prefix = if s.is_pub { "pub " } else { "" };
        if s.fields.is_empty() {
            self.line(&format!(
                "{prefix}struct {}{} {{}}",
                s.name.name,
                render_generics(&s.generics)
            ));
            return;
        }
        self.line(&format!(
            "{prefix}struct {}{} {{",
            s.name.name,
            render_generics(&s.generics)
        ));
        self.depth += 1;
        for field in &s.fields {
            self.comments_before(field.span.start);
            let prefix = if field.mutable { "var " } else { "" };
            self.line(&format!(
                "{prefix}{}: {},",
                field.name.name,
                render_type(&field.ty)
            ));
        }
        self.depth -= 1;
        self.line("}");
    }

    fn enum_item(&mut self, e: &EnumItem) {
        let prefix = if e.is_pub { "pub " } else { "" };
        if e.variants.is_empty() {
            self.line(&format!(
                "{prefix}enum {}{} {{}}",
                e.name.name,
                render_generics(&e.generics)
            ));
            return;
        }
        self.line(&format!(
            "{prefix}enum {}{} {{",
            e.name.name,
            render_generics(&e.generics)
        ));
        self.depth += 1;
        for variant in &e.variants {
            self.comments_before(variant.span.start);
            let payload = if variant.payload.is_empty() {
                String::new()
            } else {
                format!(
                    "({})",
                    variant
                        .payload
                        .iter()
                        .map(render_type)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            self.line(&format!("{}{payload},", variant.name.name));
        }
        self.depth -= 1;
        self.line("}");
    }

    // ----- blocks and statements -----

    /// Write `head { .. }suffix`, collapsing to `head {}suffix` when the block
    /// holds nothing at all.
    fn braced(&mut self, head: &str, block: &Block, suffix: &str) {
        let space = if head.is_empty() { "" } else { " " };
        if self.block_is_empty(block) {
            self.line(&format!("{head}{space}{{}}{suffix}"));
            return;
        }
        self.line(&format!("{head}{space}{{"));
        self.depth += 1;
        self.block_body(block);
        self.depth -= 1;
        self.line(&format!("}}{suffix}"));
    }

    fn block_body(&mut self, block: &Block) {
        for stmt in &block.stmts {
            self.comments_before(stmt.span.start);
            self.stmt(stmt);
        }
        if let Some(tail) = &block.tail {
            self.comments_before(tail.span.start);
            self.expr_line("", tail, "");
        }
        // Comments sitting at the end of the block, before its `}`.
        self.comments_before(block.span.end.saturating_sub(1));
    }

    fn stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::Let {
                pattern,
                ty,
                init,
                mutable,
            } => {
                let keyword = if *mutable { "var" } else { "let" };
                let annotation = ty
                    .as_ref()
                    .map(|t| format!(": {}", render_type(t)))
                    .unwrap_or_default();
                let prefix = format!("{keyword} {}{annotation} = ", render_pattern(pattern));
                self.expr_line(&prefix, init, ";");
            }
            StmtKind::Expr(expr) => {
                let suffix = if is_block_like(&expr.kind) { "" } else { ";" };
                self.expr_line("", expr, suffix);
            }
            StmtKind::Return(value) => match value {
                Some(value) => self.expr_line("return ", value, ";"),
                None => self.line("return unit;"),
            },
            StmtKind::Break => self.line("break;"),
            StmtKind::Continue => self.line("continue;"),
            StmtKind::While { cond, body } => {
                let head = format!("while {}", render_expr(cond, PREC_LOWEST));
                self.braced(&head, body, "");
            }
            StmtKind::For {
                pattern,
                iter,
                body,
            } => {
                let head = format!(
                    "for {} in {}",
                    render_pattern(pattern),
                    render_expr(iter, PREC_LOWEST)
                );
                self.braced(&head, body, "");
            }
            StmtKind::Error => {}
        }
    }

    /// Write `prefix expr suffix`, breaking onto several lines when the
    /// expression is block-shaped or the line would be too long.
    fn expr_line(&mut self, prefix: &str, expr: &Expr, suffix: &str) {
        match &expr.kind {
            ExprKind::If {
                cond,
                then_block,
                else_branch,
            } => {
                self.line(&format!("{prefix}if {} {{", render_expr(cond, PREC_LOWEST)));
                self.depth += 1;
                self.block_body(then_block);
                self.depth -= 1;
                match else_branch {
                    Some(branch) => self.else_branch(branch, suffix),
                    None => self.line(&format!("}}{suffix}")),
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                self.line(&format!(
                    "{prefix}match {} {{",
                    render_expr(scrutinee, PREC_LOWEST)
                ));
                self.depth += 1;
                for arm in arms {
                    self.comments_before(arm.span.start);
                    self.match_arm(arm);
                }
                self.depth -= 1;
                self.line(&format!("}}{suffix}"));
            }
            ExprKind::Block(block) => self.braced(prefix.trim_end(), block, suffix),
            _ => {
                let inline = format!("{prefix}{}{suffix}", render_expr(expr, PREC_LOWEST));
                if inline.chars().count() <= self.width_left() {
                    self.line(&inline);
                } else {
                    self.broken_expr(prefix, expr, suffix);
                }
            }
        }
    }

    fn else_branch(&mut self, branch: &Expr, suffix: &str) {
        match &branch.kind {
            ExprKind::If {
                cond,
                then_block,
                else_branch,
            } => {
                self.line(&format!("}} else if {} {{", render_expr(cond, PREC_LOWEST)));
                self.depth += 1;
                self.block_body(then_block);
                self.depth -= 1;
                match else_branch {
                    Some(inner) => self.else_branch(inner, suffix),
                    None => self.line(&format!("}}{suffix}")),
                }
            }
            ExprKind::Block(block) => {
                self.line("} else {");
                self.depth += 1;
                self.block_body(block);
                self.depth -= 1;
                self.line(&format!("}}{suffix}"));
            }
            other => {
                // The parser only produces `if` or a block here; anything else
                // would be a parser change, so fall back rather than panic.
                self.line("} else {");
                self.depth += 1;
                self.expr_line(
                    "",
                    &Expr {
                        kind: other.clone(),
                        span: branch.span,
                    },
                    "",
                );
                self.depth -= 1;
                self.line(&format!("}}{suffix}"));
            }
        }
    }

    fn match_arm(&mut self, arm: &MatchArm) {
        let guard = arm
            .guard
            .as_ref()
            .map(|g| format!(" if {}", render_expr(g, PREC_LOWEST)))
            .unwrap_or_default();
        let prefix = format!("{}{guard} => ", render_pattern(&arm.pattern));

        if is_block_like(&arm.body.kind) {
            self.expr_line(&prefix, &arm.body, "");
        } else {
            let inline = format!("{prefix}{},", render_expr(&arm.body, PREC_LOWEST));
            if inline.chars().count() <= self.width_left() {
                self.line(&inline);
            } else {
                self.broken_expr(&prefix, &arm.body, ",");
            }
        }
    }

    /// The multi-line form for an over-long expression: one argument or field
    /// per line. Anything without a natural break point is left long, which is
    /// a formatting shortcoming rather than a correctness one.
    fn broken_expr(&mut self, prefix: &str, expr: &Expr, suffix: &str) {
        match &expr.kind {
            ExprKind::Call { callee, args } if !args.is_empty() => {
                self.line(&format!("{prefix}{}(", render_expr(callee, PREC_POSTFIX)));
                self.depth += 1;
                for arg in args {
                    self.line(&format!("{},", render_arg(arg)));
                }
                self.depth -= 1;
                self.line(&format!("){suffix}"));
            }
            ExprKind::MethodCall {
                receiver,
                method,
                args,
            } if !args.is_empty() => {
                self.line(&format!(
                    "{prefix}{}.{}(",
                    render_expr(receiver, PREC_POSTFIX),
                    method.name
                ));
                self.depth += 1;
                for arg in args {
                    self.line(&format!("{},", render_arg(arg)));
                }
                self.depth -= 1;
                self.line(&format!("){suffix}"));
            }
            ExprKind::StructLit { path, fields } if !fields.is_empty() => {
                self.line(&format!("{prefix}{} {{", render_path(path)));
                self.depth += 1;
                for field in fields {
                    self.line(&format!(
                        "{}: {},",
                        field.name.name,
                        render_expr(&field.value, PREC_LOWEST)
                    ));
                }
                self.depth -= 1;
                self.line(&format!("}}{suffix}"));
            }
            ExprKind::ListLit(elements) if !elements.is_empty() => {
                self.line(&format!("{prefix}["));
                self.depth += 1;
                for element in elements {
                    self.line(&format!("{},", render_expr(element, PREC_LOWEST)));
                }
                self.depth -= 1;
                self.line(&format!("]{suffix}"));
            }
            _ => {
                let inline = format!("{prefix}{}{suffix}", render_expr(expr, PREC_LOWEST));
                self.line(&inline);
            }
        }
    }
}

// ------------------------------------------------------------- pure renderers

const PREC_LOWEST: u8 = 0;
const PREC_UNARY: u8 = 10;
const PREC_POSTFIX: u8 = 11;

/// Mirrors the parser's table exactly. If these ever disagree, the formatter
/// starts adding or dropping parentheses and the self-check fires.
fn binary_precedence(op: BinaryOp) -> u8 {
    match op {
        BinaryOp::Or => 1,
        BinaryOp::And => 2,
        BinaryOp::Eq
        | BinaryOp::Ne
        | BinaryOp::Lt
        | BinaryOp::Le
        | BinaryOp::Gt
        | BinaryOp::Ge
        | BinaryOp::Identity => 3,
        BinaryOp::BitOr => 4,
        BinaryOp::BitXor => 5,
        BinaryOp::BitAnd => 6,
        BinaryOp::Shl | BinaryOp::Shr => 7,
        BinaryOp::Add | BinaryOp::Sub => 8,
        BinaryOp::Mul | BinaryOp::Div | BinaryOp::Rem => 9,
    }
}

fn is_block_like(kind: &ExprKind) -> bool {
    matches!(
        kind,
        ExprKind::Block(_) | ExprKind::If { .. } | ExprKind::Match { .. }
    )
}

fn render_generics(generics: &[GenericParam]) -> String {
    if generics.is_empty() {
        return String::new();
    }
    let rendered: Vec<String> = generics
        .iter()
        .map(|g| {
            if g.bounds.is_empty() {
                g.name.name.clone()
            } else {
                let bounds: Vec<&str> = g.bounds.iter().map(|b| b.name.as_str()).collect();
                format!("{}: {}", g.name.name, bounds.join(" + "))
            }
        })
        .collect();
    format!("<{}>", rendered.join(", "))
}

fn render_path(path: &Path) -> String {
    path.segments
        .iter()
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>()
        .join(".")
}

fn render_effects(effects: &EffectSet) -> String {
    format!(
        "uses {{{}}}",
        effects
            .effects
            .iter()
            .map(render_path)
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn render_type(ty: &Type) -> String {
    match &ty.kind {
        TypeKind::Named { path, args } => {
            let base = render_path(path);
            if args.is_empty() {
                base
            } else {
                format!(
                    "{base}<{}>",
                    args.iter().map(render_type).collect::<Vec<_>>().join(", ")
                )
            }
        }
        TypeKind::Unit => "()".to_string(),
        TypeKind::Fn {
            params,
            ret,
            effects,
        } => {
            let rendered: Vec<String> = params
                .iter()
                .map(|param| match &param.name {
                    Some(name) => format!("{}: {}", name.name, render_type(&param.ty)),
                    None => render_type(&param.ty),
                })
                .collect();
            let mut text = format!("fn({}) -> {}", rendered.join(", "), render_type(ret));
            if let Some(effects) = effects {
                text.push(' ');
                text.push_str(&render_effects(effects));
            }
            text
        }
        TypeKind::Hole { name } => match name {
            Some(name) => format!("??{name}"),
            None => "??".to_string(),
        },
        TypeKind::Error => "??".to_string(),
    }
}

fn render_arg(arg: &Arg) -> String {
    match &arg.name {
        Some(name) => format!("{}: {}", name.name, render_expr(&arg.value, PREC_LOWEST)),
        None => render_expr(&arg.value, PREC_LOWEST),
    }
}

fn render_pattern(pattern: &Pattern) -> String {
    match &pattern.kind {
        PatternKind::Wildcard => "_".to_string(),
        PatternKind::Binding(ident) => ident.name.clone(),
        PatternKind::Literal(expr) => render_expr(expr, PREC_LOWEST),
        PatternKind::Path(path) => render_path(path),
        PatternKind::Variant { path, elements } => format!(
            "{}({})",
            render_path(path),
            elements
                .iter()
                .map(render_pattern)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        PatternKind::Struct { path, fields } => {
            let rendered = fields
                .iter()
                .map(|f| match &f.pattern {
                    Some(p) => format!("{}: {}", f.name.name, render_pattern(p)),
                    None => f.name.name.clone(),
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("{} {{ {rendered} }}", render_path(path))
        }
        PatternKind::Or(alternatives) => alternatives
            .iter()
            .map(render_pattern)
            .collect::<Vec<_>>()
            .join(" | "),
        PatternKind::Error => "_".to_string(),
    }
}

/// Render an expression, parenthesising when the surrounding context binds
/// tighter than this node does.
fn render_expr(expr: &Expr, min_precedence: u8) -> String {
    match &expr.kind {
        ExprKind::Int(v) | ExprKind::Float(v) | ExprKind::Str(v) | ExprKind::Char(v) => v.clone(),
        ExprKind::Bool(v) => v.to_string(),
        ExprKind::Unit => "unit".to_string(),
        ExprKind::Path(path) => render_path(path),
        ExprKind::Hole { name } => match name {
            Some(name) => format!("??{name}"),
            None => "??".to_string(),
        },

        ExprKind::Unary { op, operand } => {
            let symbol = match op {
                UnaryOp::Neg => "-",
                UnaryOp::Not => "!",
            };
            let text = format!("{symbol}{}", render_expr(operand, PREC_UNARY));
            parenthesise(text, PREC_UNARY, min_precedence)
        }

        ExprKind::Binary { op, lhs, rhs } => {
            let precedence = binary_precedence(*op);
            let text = format!(
                "{} {} {}",
                render_expr(lhs, precedence),
                op.symbol(),
                // Left associative, so the right operand needs one more.
                render_expr(rhs, precedence + 1)
            );
            parenthesise(text, precedence, min_precedence)
        }

        ExprKind::Assign { target, op, value } => {
            let symbol = match op {
                Some(op) => format!("{}=", op.symbol()),
                None => "=".to_string(),
            };
            let text = format!(
                "{} {symbol} {}",
                render_expr(target, PREC_POSTFIX),
                render_expr(value, PREC_LOWEST)
            );
            parenthesise(text, PREC_LOWEST, min_precedence)
        }

        ExprKind::Call { callee, args } => format!(
            "{}({})",
            render_expr(callee, PREC_POSTFIX),
            args.iter().map(render_arg).collect::<Vec<_>>().join(", ")
        ),
        ExprKind::MethodCall {
            receiver,
            method,
            args,
        } => format!(
            "{}.{}({})",
            render_expr(receiver, PREC_POSTFIX),
            method.name,
            args.iter().map(render_arg).collect::<Vec<_>>().join(", ")
        ),
        ExprKind::Field { receiver, name } => {
            format!("{}.{}", render_expr(receiver, PREC_POSTFIX), name.name)
        }
        ExprKind::Await(inner) => format!("{}.await", render_expr(inner, PREC_POSTFIX)),
        ExprKind::Try(inner) => format!("{}?", render_expr(inner, PREC_POSTFIX)),

        ExprKind::ListLit(elements) => format!(
            "[{}]",
            elements
                .iter()
                .map(|e| render_expr(e, PREC_LOWEST))
                .collect::<Vec<_>>()
                .join(", ")
        ),

        ExprKind::StructLit { path, fields } => {
            if fields.is_empty() {
                return format!("{} {{}}", render_path(path));
            }
            format!(
                "{} {{ {} }}",
                render_path(path),
                fields
                    .iter()
                    .map(|f| format!("{}: {}", f.name.name, render_expr(&f.value, PREC_LOWEST)))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }

        // The canonical closure: bare names, no annotations, and a
        // single-expression body without braces — the parser already
        // collapsed `{ expr }`, so rendering the body is enough.
        ExprKind::Lambda { params, body } => {
            let mut text = String::new();
            if params.is_empty() {
                text.push_str("||");
            } else {
                text.push('|');
                text.push_str(
                    &params
                        .iter()
                        .map(|p| p.name.name.clone())
                        .collect::<Vec<_>>()
                        .join(", "),
                );
                text.push('|');
            }
            text.push(' ');
            text.push_str(&render_expr(body, PREC_LOWEST));
            parenthesise(text, PREC_LOWEST, min_precedence)
        }

        // Block-shaped expressions are emitted by the writer, not rendered
        // inline. Reaching here means one appeared somewhere the writer does
        // not break, so produce something that still parses to the same tree.
        ExprKind::If { .. } | ExprKind::Match { .. } | ExprKind::Block(_) => {
            render_block_like_inline(expr)
        }

        ExprKind::Error => "??".to_string(),
    }
}

fn render_block_like_inline(expr: &Expr) -> String {
    match &expr.kind {
        ExprKind::If {
            cond,
            then_block,
            else_branch,
        } => {
            let mut text = format!(
                "if {} {}",
                render_expr(cond, PREC_LOWEST),
                render_block_inline(then_block)
            );
            if let Some(branch) = else_branch {
                text.push_str(" else ");
                text.push_str(&match &branch.kind {
                    ExprKind::Block(block) => render_block_inline(block),
                    _ => render_block_like_inline(branch),
                });
            }
            text
        }
        ExprKind::Match { scrutinee, arms } => {
            let rendered = arms
                .iter()
                .map(|arm| {
                    let guard = arm
                        .guard
                        .as_ref()
                        .map(|g| format!(" if {}", render_expr(g, PREC_LOWEST)))
                        .unwrap_or_default();
                    format!(
                        "{}{guard} => {},",
                        render_pattern(&arm.pattern),
                        render_expr(&arm.body, PREC_LOWEST)
                    )
                })
                .collect::<Vec<_>>()
                .join(" ");
            format!(
                "match {} {{ {rendered} }}",
                render_expr(scrutinee, PREC_LOWEST)
            )
        }
        ExprKind::Block(block) => render_block_inline(block),
        _ => render_expr(expr, PREC_LOWEST),
    }
}

fn render_block_inline(block: &Block) -> String {
    let mut parts: Vec<String> = Vec::new();
    for stmt in &block.stmts {
        parts.push(render_stmt_inline(stmt));
    }
    if let Some(tail) = &block.tail {
        parts.push(render_expr(tail, PREC_LOWEST));
    }
    if parts.is_empty() {
        "{}".to_string()
    } else {
        format!("{{ {} }}", parts.join(" "))
    }
}

fn render_stmt_inline(stmt: &Stmt) -> String {
    match &stmt.kind {
        StmtKind::Let {
            pattern,
            ty,
            init,
            mutable,
        } => {
            let keyword = if *mutable { "var" } else { "let" };
            let annotation = ty
                .as_ref()
                .map(|t| format!(": {}", render_type(t)))
                .unwrap_or_default();
            format!(
                "{keyword} {}{annotation} = {};",
                render_pattern(pattern),
                render_expr(init, PREC_LOWEST)
            )
        }
        StmtKind::Expr(expr) => {
            let suffix = if is_block_like(&expr.kind) { "" } else { ";" };
            format!("{}{suffix}", render_expr(expr, PREC_LOWEST))
        }
        StmtKind::Return(Some(value)) => format!("return {};", render_expr(value, PREC_LOWEST)),
        StmtKind::Return(None) => "return unit;".to_string(),
        StmtKind::Break => "break;".to_string(),
        StmtKind::Continue => "continue;".to_string(),
        StmtKind::While { cond, body } => format!(
            "while {} {}",
            render_expr(cond, PREC_LOWEST),
            render_block_inline(body)
        ),
        StmtKind::For {
            pattern,
            iter,
            body,
        } => format!(
            "for {} in {} {}",
            render_pattern(pattern),
            render_expr(iter, PREC_LOWEST),
            render_block_inline(body)
        ),
        StmtKind::Error => String::new(),
    }
}

fn parenthesise(text: String, precedence: u8, min_precedence: u8) -> String {
    if precedence < min_precedence {
        format!("({text})")
    } else {
        text
    }
}
