//! The Xenith parser.
//!
//! Hand-written recursive descent with Pratt precedence for expressions.
//! Generated parsers were rejected because diagnostic quality is much of what
//! this language is for.
//!
//! Like the lexer, the parser is total: every input yields a [`Module`].
//! Damaged regions become `Error` nodes so that later stages still see the
//! structure *around* the damage. A model mid-edit produces broken input
//! constantly, and a parser that gives up returns nothing to repair from.

use crate::ast::*;
use crate::lexer::lex;
use crate::token::{Token, TokenKind};
use xenith_diag::{DiagCode, Diagnostic, Edit, Fix, Span};

#[derive(Clone, Debug)]
pub struct Parsed {
    pub module: Module,
    pub diagnostics: Vec<Diagnostic>,
}

impl Parsed {
    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(Diagnostic::is_error)
    }
}

pub fn parse(source: &str) -> Parsed {
    let lexed = lex(source);

    // Doc comments are trivia to the parser but belong to the item that
    // follows, so record them against the significant token they precede
    // before dropping the rest of the trivia.
    let mut tokens = Vec::new();
    let mut docs_before: Vec<Vec<Span>> = Vec::new();
    let mut pending: Vec<Span> = Vec::new();
    for token in &lexed.tokens {
        match token.kind {
            TokenKind::DocComment => pending.push(token.span),
            // A blank line or an ordinary comment detaches documentation from
            // whatever follows it.
            TokenKind::LineComment => pending.clear(),
            TokenKind::Whitespace => {
                if token
                    .span
                    .slice(source)
                    .is_some_and(|t| t.matches('\n').count() > 1)
                {
                    pending.clear();
                }
            }
            _ => {
                tokens.push(*token);
                docs_before.push(std::mem::take(&mut pending));
            }
        }
    }

    let mut parser = Parser {
        source,
        tokens,
        docs_before,
        pos: 0,
        diagnostics: lexed.diagnostics,
        no_struct_literal: false,
    };
    let module = parser.module();
    Parsed {
        module,
        diagnostics: parser.diagnostics,
    }
}

struct Parser<'a> {
    source: &'a str,
    /// Significant tokens only, always ending in `Eof`.
    tokens: Vec<Token>,
    docs_before: Vec<Vec<Span>>,
    pos: usize,
    diagnostics: Vec<Diagnostic>,
    /// While parsing the condition of `if`/`while` or a `match` scrutinee, a
    /// `{` opens the body rather than a struct literal. Without this,
    /// `if ready { .. }` parses `ready { .. }` as a struct literal and the
    /// error lands nowhere near the mistake.
    no_struct_literal: bool,
}

impl<'a> Parser<'a> {
    // ----- cursor -----

    fn peek(&self) -> TokenKind {
        self.tokens[self.pos].kind
    }

    fn peek_ahead(&self, n: usize) -> TokenKind {
        self.tokens
            .get(self.pos + n)
            .map(|t| t.kind)
            .unwrap_or(TokenKind::Eof)
    }

    fn at(&self, kind: TokenKind) -> bool {
        self.peek() == kind
    }

    fn at_end(&self) -> bool {
        self.at(TokenKind::Eof)
    }

    fn current(&self) -> Token {
        self.tokens[self.pos]
    }

    fn span(&self) -> Span {
        self.current().span
    }

    /// Span of the token just consumed, for closing a node.
    fn prev_span(&self) -> Span {
        self.tokens[self.pos.saturating_sub(1)].span
    }

    fn bump(&mut self) -> Token {
        let token = self.current();
        if !self.at_end() {
            self.pos += 1;
        }
        token
    }

    fn eat(&mut self, kind: TokenKind) -> bool {
        if self.at(kind) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn text(&self, span: Span) -> String {
        span.slice(self.source).unwrap_or("").to_string()
    }

    fn take_docs(&self) -> Vec<Span> {
        self.docs_before.get(self.pos).cloned().unwrap_or_default()
    }

    // ----- diagnostics -----

    fn error(&mut self, code: DiagCode, span: Span, message: impl Into<String>) {
        self.diagnostics
            .push(Diagnostic::error(code, span, message));
    }

    /// Require `kind`. On failure, report and — for punctuation that was simply
    /// omitted — attach an insertion fix. The token is *not* consumed, so the
    /// caller keeps its place in the stream.
    fn expect(&mut self, kind: TokenKind) -> bool {
        if self.eat(kind) {
            return true;
        }
        let found = self.peek();
        let span = self.span();
        let message = format!("expected {}, found {}", kind.describe(), found.describe());

        let mut diagnostic = if kind == TokenKind::Semi {
            Diagnostic::error(DiagCode::MissingSemicolon, span, "expected `;`")
        } else {
            Diagnostic::error(DiagCode::ExpectedToken, span, message)
        };

        if let Some(literal) = punctuation_text(kind) {
            // Insert at the end of the previous token: that is where the
            // missing punctuation belongs, not at the start of the token that
            // exposed the problem.
            diagnostic = diagnostic.with_fix(Fix::single(
                format!("insert `{literal}`"),
                Edit::insert(self.prev_span().end, literal),
            ));
        }
        self.diagnostics.push(diagnostic);
        false
    }

    fn expect_ident(&mut self) -> Ident {
        if self.at(TokenKind::Ident) {
            let token = self.bump();
            return Ident::new(self.text(token.span), token.span);
        }
        let span = self.span();
        self.error(
            DiagCode::ExpectedToken,
            span,
            format!("expected identifier, found {}", self.peek().describe()),
        );
        Ident::new("", Span::at(span.start))
    }

    // ----- recovery -----

    /// Skip to the start of the next declaration.
    fn recover_to_item(&mut self) {
        while !self.at_end() && !starts_item(self.peek()) {
            self.bump();
        }
    }

    /// Skip to the end of the current statement, consuming the `;` if found.
    fn recover_to_statement_end(&mut self) {
        let mut depth = 0i32;
        while !self.at_end() {
            match self.peek() {
                TokenKind::LBrace | TokenKind::LParen | TokenKind::LBracket => depth += 1,
                TokenKind::RBrace | TokenKind::RParen | TokenKind::RBracket => {
                    if depth == 0 {
                        return;
                    }
                    depth -= 1;
                }
                TokenKind::Semi if depth == 0 => {
                    self.bump();
                    return;
                }
                _ if depth == 0 && starts_item(self.peek()) => return,
                _ => {}
            }
            self.bump();
        }
    }

    // ----- module -----

    fn module(&mut self) -> Module {
        let start = self.span();
        let mut items = Vec::new();
        while !self.at_end() {
            let before = self.pos;
            items.push(self.item());
            if self.pos == before {
                // The item parser made no progress. Consume one token so the
                // loop always terminates.
                self.bump();
            }
        }
        Module {
            items,
            span: start.to(self.prev_span()),
        }
    }

    fn item(&mut self) -> Item {
        let docs = self.take_docs();
        let start = self.span();

        let kind = match self.peek() {
            TokenKind::Use => ItemKind::Use(self.use_item()),
            TokenKind::Const => ItemKind::Const(self.const_item()),
            TokenKind::Fn | TokenKind::Async => ItemKind::Fn(self.fn_item()),
            TokenKind::Struct => ItemKind::Struct(self.struct_item()),
            TokenKind::Enum => ItemKind::Enum(self.enum_item()),
            other => {
                self.error(
                    DiagCode::ExpectedItem,
                    start,
                    format!("expected a declaration, found {}", other.describe()),
                );
                self.bump();
                self.recover_to_item();
                ItemKind::Error
            }
        };

        Item {
            kind,
            docs,
            span: start.to(self.prev_span()),
        }
    }

    fn use_item(&mut self) -> UseItem {
        self.bump(); // `use`
        let path = self.path();
        self.expect(TokenKind::Semi);
        UseItem { path }
    }

    fn const_item(&mut self) -> ConstItem {
        self.bump(); // `const`
        let name = self.expect_ident();
        self.expect(TokenKind::Colon);
        let ty = self.ty();
        self.expect(TokenKind::Assign);
        let value = self.expr();
        self.expect(TokenKind::Semi);
        ConstItem { name, ty, value }
    }

    fn fn_item(&mut self) -> FnItem {
        let is_async = self.eat(TokenKind::Async);
        self.expect(TokenKind::Fn);
        let name = self.expect_ident();
        let generics = self.generic_params();

        let mut params = Vec::new();
        if self.expect(TokenKind::LParen) {
            while !self.at(TokenKind::RParen) && !self.at_end() {
                let param_start = self.span();
                let param_name = self.expect_ident();
                self.expect(TokenKind::Colon);
                let ty = self.ty();
                params.push(Param {
                    name: param_name,
                    ty,
                    span: param_start.to(self.prev_span()),
                });
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }
            self.expect(TokenKind::RParen);
        }

        let return_type = if self.eat(TokenKind::Arrow) {
            Some(self.ty())
        } else {
            None
        };

        let effects = self.effect_set();

        let body = if self.at(TokenKind::LBrace) {
            Some(self.block())
        } else {
            self.expect(TokenKind::LBrace);
            None
        };

        FnItem {
            name,
            is_async,
            generics,
            params,
            return_type,
            effects,
            body,
        }
    }

    /// `uses {Fs.read, Net.send}`. Absent means the empty set.
    fn effect_set(&mut self) -> Option<EffectSet> {
        if !self.at(TokenKind::Uses) {
            return None;
        }
        let start = self.span();
        self.bump();
        self.expect(TokenKind::LBrace);
        let mut effects = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at_end() {
            effects.push(self.path());
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RBrace);
        Some(EffectSet {
            effects,
            span: start.to(self.prev_span()),
        })
    }

    fn generic_params(&mut self) -> Vec<Ident> {
        let mut generics = Vec::new();
        if !self.eat(TokenKind::Lt) {
            return generics;
        }
        while !self.at(TokenKind::Gt) && !self.at_end() {
            generics.push(self.expect_ident());
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::Gt);
        generics
    }

    fn struct_item(&mut self) -> StructItem {
        self.bump(); // `struct`
        let name = self.expect_ident();
        let generics = self.generic_params();
        let mut fields = Vec::new();

        if self.expect(TokenKind::LBrace) {
            while !self.at(TokenKind::RBrace) && !self.at_end() {
                let docs = self.take_docs();
                let field_start = self.span();
                let mutable = self.eat(TokenKind::Var);
                let field_name = self.expect_ident();
                self.expect(TokenKind::Colon);
                let ty = self.ty();
                fields.push(FieldDef {
                    name: field_name,
                    ty,
                    mutable,
                    docs,
                    span: field_start.to(self.prev_span()),
                });
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }
            self.expect(TokenKind::RBrace);
        }

        StructItem {
            name,
            generics,
            fields,
        }
    }

    fn enum_item(&mut self) -> EnumItem {
        self.bump(); // `enum`
        let name = self.expect_ident();
        let generics = self.generic_params();
        let mut variants = Vec::new();

        if self.expect(TokenKind::LBrace) {
            while !self.at(TokenKind::RBrace) && !self.at_end() {
                let docs = self.take_docs();
                let variant_start = self.span();
                let variant_name = self.expect_ident();
                let mut payload = Vec::new();
                if self.eat(TokenKind::LParen) {
                    while !self.at(TokenKind::RParen) && !self.at_end() {
                        payload.push(self.ty());
                        if !self.eat(TokenKind::Comma) {
                            break;
                        }
                    }
                    self.expect(TokenKind::RParen);
                }
                variants.push(VariantDef {
                    name: variant_name,
                    payload,
                    docs,
                    span: variant_start.to(self.prev_span()),
                });
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }
            self.expect(TokenKind::RBrace);
        }

        EnumItem {
            name,
            generics,
            variants,
        }
    }

    // ----- paths and types -----

    fn path(&mut self) -> Path {
        let start = self.span();
        let mut segments = vec![self.expect_ident()];
        while self.at(TokenKind::Dot) && self.peek_ahead(1) == TokenKind::Ident {
            self.bump();
            segments.push(self.expect_ident());
        }
        Path {
            segments,
            span: start.to(self.prev_span()),
        }
    }

    fn ty(&mut self) -> Type {
        let start = self.span();

        match self.peek() {
            TokenKind::Hole => {
                let token = self.bump();
                let name = hole_name(&self.text(token.span));
                Type {
                    kind: TypeKind::Hole { name },
                    span: token.span,
                }
            }
            TokenKind::LParen => {
                self.bump();
                if self.eat(TokenKind::RParen) {
                    return Type {
                        kind: TypeKind::Unit,
                        span: start.to(self.prev_span()),
                    };
                }
                // Parenthesised type.
                let inner = self.ty();
                self.expect(TokenKind::RParen);
                inner
            }
            TokenKind::Fn => {
                self.bump();
                let mut params = Vec::new();
                if self.expect(TokenKind::LParen) {
                    while !self.at(TokenKind::RParen) && !self.at_end() {
                        params.push(self.ty());
                        if !self.eat(TokenKind::Comma) {
                            break;
                        }
                    }
                    self.expect(TokenKind::RParen);
                }
                let ret = if self.eat(TokenKind::Arrow) {
                    Box::new(self.ty())
                } else {
                    Box::new(Type {
                        kind: TypeKind::Unit,
                        span: self.prev_span(),
                    })
                };
                let effects = self.effect_set();
                Type {
                    kind: TypeKind::Fn {
                        params,
                        ret,
                        effects,
                    },
                    span: start.to(self.prev_span()),
                }
            }
            TokenKind::Ident => {
                let path = self.path();
                let mut args = Vec::new();
                if self.eat(TokenKind::Lt) {
                    while !self.at(TokenKind::Gt) && !self.at_end() {
                        args.push(self.ty());
                        if !self.eat(TokenKind::Comma) {
                            break;
                        }
                    }
                    self.expect(TokenKind::Gt);
                }
                Type {
                    kind: TypeKind::Named { path, args },
                    span: start.to(self.prev_span()),
                }
            }
            other => {
                self.error(
                    DiagCode::ExpectedType,
                    start,
                    format!("expected a type, found {}", other.describe()),
                );
                Type {
                    kind: TypeKind::Error,
                    span: start,
                }
            }
        }
    }

    // ----- blocks and statements -----

    fn block(&mut self) -> Block {
        let start = self.span();
        if !self.expect(TokenKind::LBrace) {
            return Block {
                stmts: Vec::new(),
                tail: None,
                span: start,
            };
        }

        let mut stmts = Vec::new();
        let mut tail = None;

        while !self.at(TokenKind::RBrace) && !self.at_end() {
            let before = self.pos;

            // Statement keywords are unambiguous; anything else starts an
            // expression, which is either a statement or the block's value.
            match self.peek() {
                TokenKind::Let | TokenKind::Var => stmts.push(self.let_stmt()),
                TokenKind::Return => stmts.push(self.return_stmt()),
                TokenKind::Break | TokenKind::Continue => stmts.push(self.jump_stmt()),
                TokenKind::While => stmts.push(self.while_stmt()),
                TokenKind::For => stmts.push(self.for_stmt()),
                _ => {
                    let expr_start = self.span();
                    let expr = self.expr();
                    if self.eat(TokenKind::Semi) {
                        stmts.push(Stmt {
                            kind: StmtKind::Expr(expr),
                            span: expr_start.to(self.prev_span()),
                        });
                    } else if self.at(TokenKind::RBrace) {
                        // No `;` and the block is ending: this is its value.
                        tail = Some(Box::new(expr));
                        break;
                    } else if block_like(&expr.kind) {
                        // `if`, `match` and bare blocks used for their effects
                        // do not need a terminator.
                        stmts.push(Stmt {
                            kind: StmtKind::Expr(expr),
                            span: expr_start.to(self.prev_span()),
                        });
                    } else {
                        self.expect(TokenKind::Semi);
                        stmts.push(Stmt {
                            kind: StmtKind::Expr(expr),
                            span: expr_start.to(self.prev_span()),
                        });
                        self.recover_to_statement_end();
                    }
                }
            }

            if self.pos == before {
                self.bump();
            }
        }

        self.expect(TokenKind::RBrace);
        Block {
            stmts,
            tail,
            span: start.to(self.prev_span()),
        }
    }

    fn let_stmt(&mut self) -> Stmt {
        let start = self.span();
        let mutable = self.at(TokenKind::Var);
        self.bump(); // `let` or `var`
        let pattern = self.pattern();
        let ty = if self.eat(TokenKind::Colon) {
            Some(self.ty())
        } else {
            None
        };
        self.expect(TokenKind::Assign);
        let init = self.expr();
        self.expect(TokenKind::Semi);
        Stmt {
            kind: StmtKind::Let {
                pattern,
                ty,
                init,
                mutable,
            },
            span: start.to(self.prev_span()),
        }
    }

    fn return_stmt(&mut self) -> Stmt {
        let start = self.span();
        self.bump(); // `return`
        let value = if self.at(TokenKind::Semi) {
            // The grammar requires an operand. Report it here, with the fix,
            // rather than refusing to parse — a bare `return` is a mistake we
            // can name precisely.
            let span = start.to(self.span());
            self.diagnostics.push(
                Diagnostic::error(
                    DiagCode::ExpectedExpression,
                    span,
                    "`return` requires a value",
                )
                .with_fix(Fix::single(
                    "return `unit` explicitly",
                    Edit::insert(self.prev_span().end, " unit"),
                )),
            );
            None
        } else {
            Some(self.expr())
        };
        self.expect(TokenKind::Semi);
        Stmt {
            kind: StmtKind::Return(value),
            span: start.to(self.prev_span()),
        }
    }

    fn jump_stmt(&mut self) -> Stmt {
        let start = self.span();
        let kind = if self.at(TokenKind::Break) {
            StmtKind::Break
        } else {
            StmtKind::Continue
        };
        self.bump();
        self.expect(TokenKind::Semi);
        Stmt {
            kind,
            span: start.to(self.prev_span()),
        }
    }

    fn while_stmt(&mut self) -> Stmt {
        let start = self.span();
        self.bump(); // `while`
        let cond = self.condition_expr();
        let body = self.block();
        Stmt {
            kind: StmtKind::While { cond, body },
            span: start.to(self.prev_span()),
        }
    }

    fn for_stmt(&mut self) -> Stmt {
        let start = self.span();
        self.bump(); // `for`
        let pattern = self.pattern();
        self.expect(TokenKind::In);
        let iter = self.condition_expr();
        let body = self.block();
        Stmt {
            kind: StmtKind::For {
                pattern,
                iter,
                body,
            },
            span: start.to(self.prev_span()),
        }
    }

    /// An expression in a position immediately followed by a block.
    fn condition_expr(&mut self) -> Expr {
        let saved = self.no_struct_literal;
        self.no_struct_literal = true;
        let expr = self.expr();
        self.no_struct_literal = saved;
        expr
    }

    // ----- expressions -----

    fn expr(&mut self) -> Expr {
        self.assignment()
    }

    fn assignment(&mut self) -> Expr {
        let target = self.binary(0);

        let op = match self.peek() {
            TokenKind::Assign => None,
            TokenKind::PlusAssign => Some(BinaryOp::Add),
            TokenKind::MinusAssign => Some(BinaryOp::Sub),
            TokenKind::StarAssign => Some(BinaryOp::Mul),
            TokenKind::SlashAssign => Some(BinaryOp::Div),
            TokenKind::PercentAssign => Some(BinaryOp::Rem),
            _ => return target,
        };
        self.bump();

        // Right associative: `a = b = c` is `a = (b = c)`.
        let value = self.assignment();
        let span = target.span.to(value.span);
        Expr {
            kind: ExprKind::Assign {
                target: Box::new(target),
                op,
                value: Box::new(value),
            },
            span,
        }
    }

    /// Pratt loop. Precedence follows Rust exactly, including bitwise binding
    /// tighter than comparison — the C ordering there is a well-known trap and
    /// deviating from Rust would cost transfer for no gain.
    fn binary(&mut self, min_precedence: u8) -> Expr {
        let mut lhs = self.unary();

        while let Some((op, precedence, width)) = self.peek_binary_op() {
            if precedence < min_precedence {
                break;
            }
            for _ in 0..width {
                self.bump();
            }
            // All binary operators here are left associative.
            let rhs = self.binary(precedence + 1);
            let span = lhs.span.to(rhs.span);
            lhs = Expr {
                kind: ExprKind::Binary {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
                span,
            };
        }

        lhs
    }

    /// The operator at the cursor, its precedence, and how many tokens it
    /// spans. Shifts are two tokens: the lexer never joins `<` `<` or `>` `>`,
    /// because doing so would break nested generics.
    fn peek_binary_op(&self) -> Option<(BinaryOp, u8, usize)> {
        let adjacent = |a: usize, b: usize| -> bool {
            match (self.tokens.get(a), self.tokens.get(b)) {
                (Some(x), Some(y)) => x.span.end == y.span.start,
                _ => false,
            }
        };

        let op = match self.peek() {
            TokenKind::OrOr => (BinaryOp::Or, 1, 1),
            TokenKind::AndAnd => (BinaryOp::And, 2, 1),

            TokenKind::EqEq => (BinaryOp::Eq, 3, 1),
            TokenKind::NotEq => (BinaryOp::Ne, 3, 1),
            TokenKind::Lt => {
                if self.peek_ahead(1) == TokenKind::Lt && adjacent(self.pos, self.pos + 1) {
                    (BinaryOp::Shl, 7, 2)
                } else {
                    (BinaryOp::Lt, 3, 1)
                }
            }
            TokenKind::Gt => {
                if self.peek_ahead(1) == TokenKind::Gt && adjacent(self.pos, self.pos + 1) {
                    (BinaryOp::Shr, 7, 2)
                } else {
                    (BinaryOp::Gt, 3, 1)
                }
            }
            TokenKind::LtEq => (BinaryOp::Le, 3, 1),
            TokenKind::GtEq => (BinaryOp::Ge, 3, 1),
            TokenKind::Is => (BinaryOp::Identity, 3, 1),

            TokenKind::Pipe => (BinaryOp::BitOr, 4, 1),
            TokenKind::Caret => (BinaryOp::BitXor, 5, 1),
            TokenKind::Amp => (BinaryOp::BitAnd, 6, 1),

            TokenKind::Plus => (BinaryOp::Add, 8, 1),
            TokenKind::Minus => (BinaryOp::Sub, 8, 1),

            TokenKind::Star => (BinaryOp::Mul, 9, 1),
            TokenKind::Slash => (BinaryOp::Div, 9, 1),
            TokenKind::Percent => (BinaryOp::Rem, 9, 1),

            _ => return None,
        };
        Some(op)
    }

    fn unary(&mut self) -> Expr {
        let start = self.span();
        let op = match self.peek() {
            TokenKind::Minus => UnaryOp::Neg,
            TokenKind::Bang => UnaryOp::Not,
            _ => return self.postfix(),
        };
        self.bump();
        let operand = self.unary();
        let span = start.to(operand.span);
        Expr {
            kind: ExprKind::Unary {
                op,
                operand: Box::new(operand),
            },
            span,
        }
    }

    fn postfix(&mut self) -> Expr {
        let mut expr = self.primary();

        loop {
            match self.peek() {
                TokenKind::Dot => {
                    self.bump();
                    if self.eat(TokenKind::Await) {
                        let span = expr.span.to(self.prev_span());
                        expr = Expr {
                            kind: ExprKind::Await(Box::new(expr)),
                            span,
                        };
                        continue;
                    }
                    let name = self.expect_ident();
                    if self.at(TokenKind::LParen) {
                        let args = self.call_args();
                        let span = expr.span.to(self.prev_span());
                        expr = Expr {
                            kind: ExprKind::MethodCall {
                                receiver: Box::new(expr),
                                method: name,
                                args,
                            },
                            span,
                        };
                    } else {
                        let span = expr.span.to(name.span);
                        expr = Expr {
                            kind: ExprKind::Field {
                                receiver: Box::new(expr),
                                name,
                            },
                            span,
                        };
                    }
                }
                TokenKind::LParen => {
                    let args = self.call_args();
                    let span = expr.span.to(self.prev_span());
                    expr = Expr {
                        kind: ExprKind::Call {
                            callee: Box::new(expr),
                            args,
                        },
                        span,
                    };
                }
                TokenKind::Question => {
                    self.bump();
                    let span = expr.span.to(self.prev_span());
                    expr = Expr {
                        kind: ExprKind::Try(Box::new(expr)),
                        span,
                    };
                }
                _ => break,
            }
        }

        expr
    }

    fn call_args(&mut self) -> Vec<Arg> {
        let mut args = Vec::new();
        if !self.expect(TokenKind::LParen) {
            return args;
        }

        // Struct-literal suppression applies to the condition itself, not to
        // anything nested inside brackets within it.
        let saved = self.no_struct_literal;
        self.no_struct_literal = false;

        while !self.at(TokenKind::RParen) && !self.at_end() {
            let arg_start = self.span();
            // `name: value` is a named argument; a bare expression is
            // positional. Both parse — the rule requiring names for two or
            // more arguments is enforced once the callee is known, so the
            // diagnostic can name the parameters.
            let name = if self.at(TokenKind::Ident) && self.peek_ahead(1) == TokenKind::Colon {
                let token = self.bump();
                self.bump(); // `:`
                Some(Ident::new(self.text(token.span), token.span))
            } else {
                None
            };
            let value = self.expr();
            args.push(Arg {
                name,
                value,
                span: arg_start.to(self.prev_span()),
            });
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }

        self.no_struct_literal = saved;
        self.expect(TokenKind::RParen);
        args
    }

    fn primary(&mut self) -> Expr {
        let start = self.span();

        match self.peek() {
            TokenKind::Int => {
                let token = self.bump();
                Expr {
                    kind: ExprKind::Int(self.text(token.span)),
                    span: token.span,
                }
            }
            TokenKind::Float => {
                let token = self.bump();
                Expr {
                    kind: ExprKind::Float(self.text(token.span)),
                    span: token.span,
                }
            }
            TokenKind::Str => {
                let token = self.bump();
                Expr {
                    kind: ExprKind::Str(self.text(token.span)),
                    span: token.span,
                }
            }
            TokenKind::Char => {
                let token = self.bump();
                Expr {
                    kind: ExprKind::Char(self.text(token.span)),
                    span: token.span,
                }
            }
            TokenKind::True | TokenKind::False => {
                let value = self.at(TokenKind::True);
                let token = self.bump();
                Expr {
                    kind: ExprKind::Bool(value),
                    span: token.span,
                }
            }
            TokenKind::Unit => {
                let token = self.bump();
                Expr {
                    kind: ExprKind::Unit,
                    span: token.span,
                }
            }
            TokenKind::Hole => {
                let token = self.bump();
                let name = hole_name(&self.text(token.span));
                Expr {
                    kind: ExprKind::Hole { name },
                    span: token.span,
                }
            }
            TokenKind::LParen => {
                self.bump();
                if self.eat(TokenKind::RParen) {
                    return Expr {
                        kind: ExprKind::Unit,
                        span: start.to(self.prev_span()),
                    };
                }
                let saved = self.no_struct_literal;
                self.no_struct_literal = false;
                let inner = self.expr();
                self.no_struct_literal = saved;
                self.expect(TokenKind::RParen);
                inner
            }
            TokenKind::LBrace => {
                let block = self.block();
                let span = block.span;
                Expr {
                    kind: ExprKind::Block(block),
                    span,
                }
            }
            TokenKind::If => self.if_expr(),
            TokenKind::Match => self.match_expr(),
            TokenKind::Move | TokenKind::Pipe | TokenKind::OrOr => self.lambda(false),
            TokenKind::Async => {
                self.bump();
                self.lambda(true)
            }
            TokenKind::Ident => {
                // Exactly one segment. `Rank.Gold` and `player.score` are
                // syntactically identical, so the parser does not try to tell
                // them apart: both become field access over a single-segment
                // path, and name resolution decides which is which. Consuming
                // the dots here instead would make every field access parse as
                // a path and no field access would ever be produced.
                let token = self.bump();
                let ident = Ident::new(self.text(token.span), token.span);
                let path = Path {
                    segments: vec![ident],
                    span: token.span,
                };
                if self.at(TokenKind::LBrace) && !self.no_struct_literal {
                    return self.struct_literal(path);
                }
                Expr {
                    kind: ExprKind::Path(path),
                    span: token.span,
                }
            }
            other => {
                self.error(
                    DiagCode::ExpectedExpression,
                    start,
                    format!("expected an expression, found {}", other.describe()),
                );
                Expr {
                    kind: ExprKind::Error,
                    span: start,
                }
            }
        }
    }

    fn if_expr(&mut self) -> Expr {
        let start = self.span();
        self.bump(); // `if`
        let cond = self.condition_expr();
        let then_block = self.block();

        let else_branch = if self.eat(TokenKind::Else) {
            if self.at(TokenKind::If) {
                Some(Box::new(self.if_expr()))
            } else {
                let block = self.block();
                let span = block.span;
                Some(Box::new(Expr {
                    kind: ExprKind::Block(block),
                    span,
                }))
            }
        } else {
            None
        };

        Expr {
            kind: ExprKind::If {
                cond: Box::new(cond),
                then_block,
                else_branch,
            },
            span: start.to(self.prev_span()),
        }
    }

    fn match_expr(&mut self) -> Expr {
        let start = self.span();
        self.bump(); // `match`
        let scrutinee = self.condition_expr();
        let mut arms = Vec::new();

        if self.expect(TokenKind::LBrace) {
            while !self.at(TokenKind::RBrace) && !self.at_end() {
                let before = self.pos;
                let arm_start = self.span();
                let pattern = self.pattern();
                let guard = if self.eat(TokenKind::If) {
                    Some(self.condition_expr())
                } else {
                    None
                };
                self.expect(TokenKind::FatArrow);
                let body = self.expr();
                arms.push(MatchArm {
                    pattern,
                    guard,
                    body,
                    span: arm_start.to(self.prev_span()),
                });
                // A trailing comma is optional after a block-shaped arm.
                if !self.eat(TokenKind::Comma) && !self.at(TokenKind::RBrace) {
                    let is_block = matches!(
                        arms.last().map(|a| &a.body.kind),
                        Some(ExprKind::Block(_) | ExprKind::If { .. } | ExprKind::Match { .. })
                    );
                    if !is_block {
                        self.expect(TokenKind::Comma);
                        self.recover_to_statement_end();
                    }
                }
                if self.pos == before {
                    self.bump();
                }
            }
            self.expect(TokenKind::RBrace);
        }

        Expr {
            kind: ExprKind::Match {
                scrutinee: Box::new(scrutinee),
                arms,
            },
            span: start.to(self.prev_span()),
        }
    }

    fn struct_literal(&mut self, path: Path) -> Expr {
        let start = path.span;
        self.bump(); // `{`
        let mut fields = Vec::new();

        let saved = self.no_struct_literal;
        self.no_struct_literal = false;

        while !self.at(TokenKind::RBrace) && !self.at_end() {
            let field_start = self.span();
            let name = self.expect_ident();
            self.expect(TokenKind::Colon);
            let value = self.expr();
            fields.push(FieldInit {
                name,
                value,
                span: field_start.to(self.prev_span()),
            });
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }

        self.no_struct_literal = saved;
        self.expect(TokenKind::RBrace);

        Expr {
            kind: ExprKind::StructLit { path, fields },
            span: start.to(self.prev_span()),
        }
    }

    fn lambda(&mut self, is_async: bool) -> Expr {
        let start = self.span();
        let is_move = self.eat(TokenKind::Move);
        let mut params = Vec::new();

        if self.eat(TokenKind::OrOr) {
            // `||` is an empty parameter list, not an or-operator.
        } else if self.expect(TokenKind::Pipe) {
            while !self.at(TokenKind::Pipe) && !self.at_end() {
                let param_start = self.span();
                let name = self.expect_ident();
                self.expect(TokenKind::Colon);
                let ty = self.ty();
                params.push(Param {
                    name,
                    ty,
                    span: param_start.to(self.prev_span()),
                });
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }
            self.expect(TokenKind::Pipe);
        }

        let body = self.expr();
        let span = start.to(body.span);
        Expr {
            kind: ExprKind::Lambda {
                params,
                is_move,
                is_async,
                body: Box::new(body),
            },
            span,
        }
    }

    // ----- patterns -----

    fn pattern(&mut self) -> Pattern {
        let first = self.pattern_single();
        if !self.at(TokenKind::Pipe) {
            return first;
        }
        let start = first.span;
        let mut alternatives = vec![first];
        while self.eat(TokenKind::Pipe) {
            alternatives.push(self.pattern_single());
        }
        Pattern {
            kind: PatternKind::Or(alternatives),
            span: start.to(self.prev_span()),
        }
    }

    fn pattern_single(&mut self) -> Pattern {
        let start = self.span();

        match self.peek() {
            TokenKind::Underscore => {
                let token = self.bump();
                Pattern {
                    kind: PatternKind::Wildcard,
                    span: token.span,
                }
            }
            TokenKind::Int | TokenKind::Float | TokenKind::Str | TokenKind::Char => {
                let expr = self.primary();
                let span = expr.span;
                Pattern {
                    kind: PatternKind::Literal(expr),
                    span,
                }
            }
            TokenKind::True | TokenKind::False | TokenKind::Unit => {
                let expr = self.primary();
                let span = expr.span;
                Pattern {
                    kind: PatternKind::Literal(expr),
                    span,
                }
            }
            TokenKind::Minus => {
                let expr = self.unary();
                let span = expr.span;
                Pattern {
                    kind: PatternKind::Literal(expr),
                    span,
                }
            }
            TokenKind::Ident => {
                let path = self.path();

                if self.at(TokenKind::LParen) {
                    self.bump();
                    let mut elements = Vec::new();
                    while !self.at(TokenKind::RParen) && !self.at_end() {
                        elements.push(self.pattern());
                        if !self.eat(TokenKind::Comma) {
                            break;
                        }
                    }
                    self.expect(TokenKind::RParen);
                    return Pattern {
                        kind: PatternKind::Variant { path, elements },
                        span: start.to(self.prev_span()),
                    };
                }

                if self.at(TokenKind::LBrace) && !self.no_struct_literal {
                    self.bump();
                    let mut fields = Vec::new();
                    while !self.at(TokenKind::RBrace) && !self.at_end() {
                        let field_start = self.span();
                        let name = self.expect_ident();
                        let pattern = if self.eat(TokenKind::Colon) {
                            Some(self.pattern())
                        } else {
                            None
                        };
                        fields.push(FieldPattern {
                            name,
                            pattern,
                            span: field_start.to(self.prev_span()),
                        });
                        if !self.eat(TokenKind::Comma) {
                            break;
                        }
                    }
                    self.expect(TokenKind::RBrace);
                    return Pattern {
                        kind: PatternKind::Struct { path, fields },
                        span: start.to(self.prev_span()),
                    };
                }

                // A single lowercase-style segment binds; anything dotted names
                // a variant or constant.
                if path.segments.len() == 1 {
                    let ident = path.segments.into_iter().next().expect("one segment");
                    let span = ident.span;
                    Pattern {
                        kind: PatternKind::Binding(ident),
                        span,
                    }
                } else {
                    let span = path.span;
                    Pattern {
                        kind: PatternKind::Path(path),
                        span,
                    }
                }
            }
            other => {
                self.error(
                    DiagCode::ExpectedPattern,
                    start,
                    format!("expected a pattern, found {}", other.describe()),
                );
                Pattern {
                    kind: PatternKind::Error,
                    span: start,
                }
            }
        }
    }
}

/// Expressions whose syntax already ends in `}` and so need no `;` when used
/// as a statement.
fn block_like(kind: &ExprKind) -> bool {
    matches!(
        kind,
        ExprKind::Block(_) | ExprKind::If { .. } | ExprKind::Match { .. }
    )
}

fn starts_item(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Use
            | TokenKind::Const
            | TokenKind::Fn
            | TokenKind::Async
            | TokenKind::Struct
            | TokenKind::Enum
    )
}

/// The literal text of a token that can be inserted as a fix. Only punctuation
/// qualifies: inserting a missing identifier would be a guess.
fn punctuation_text(kind: TokenKind) -> Option<&'static str> {
    let text = match kind {
        TokenKind::Semi => ";",
        TokenKind::Comma => ",",
        TokenKind::Colon => ":",
        TokenKind::LParen => "(",
        TokenKind::RParen => ")",
        TokenKind::LBrace => "{",
        TokenKind::RBrace => "}",
        TokenKind::LBracket => "[",
        TokenKind::RBracket => "]",
        TokenKind::Arrow => "->",
        TokenKind::FatArrow => "=>",
        TokenKind::Assign => "=",
        TokenKind::Lt => "<",
        TokenKind::Gt => ">",
        TokenKind::Pipe => "|",
        _ => return None,
    };
    Some(text)
}

/// `??` yields `None`; `??name` yields `Some("name")`.
fn hole_name(text: &str) -> Option<String> {
    let name = text.strip_prefix("??")?;
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}
