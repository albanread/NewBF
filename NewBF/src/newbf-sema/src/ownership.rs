//! MS-T5 — compile-time delete-flow: provable double-free analysis (Track B).
//!
//! A **pure-sema** pass (no IR, no LLVM) that tracks user-written
//! `new ClassType()` locals through a small ownership lattice and diagnoses two
//! kinds of *provable* mistake:
//!
//!  * a **double-`delete`** of the same binding with no intervening
//!    reassignment, and
//!  * a **`delete` of a `scope`-bound binding** (the `scope` lifetime cleanup
//!    will also free it — a guaranteed double free).
//!
//! It runs inside [`crate::analyze`] **after** `resolve_and_check`, appending to
//! `Program.diagnostics` (memory-safety.md §B0). The [`DefGraph`] carries no
//! method bodies, so this pass **re-walks the raw `CompUnit` ASTs** in `files`
//! (the same sources lowering walks) and builds a **minimal per-body local
//! type/state map**. It sees only the **user sources** — the corlib prelude is
//! prepended later, inside lowering, so library code is never analysed here.
//!
//! ## What is tracked (§B1)
//!
//! A local binding is tracked **only** when it is initialized from a
//! **user-written `new ClassType()`** (or `scope ClassType()`) where
//! `ClassType` resolves to a user-declared **class** (`TypeKindD::Class`).
//! Compiler-synthesized allocations — String interpolation/literals,
//! target-typed array/collection literals, closure environments — and value
//! types are **never** tracked. Those are the runtime guard's job.
//!
//! ## The lattice (§B2)
//!
//! Per tracked binding ∈ `{Owned, OwnedScope, Deleted}` (an untracked binding is
//! simply absent from the map):
//!
//!  * `let p = new T()`        → `Owned`
//!  * `let p = scope T()`      → `OwnedScope`
//!  * `delete p` when `Owned`  → `Deleted` (no diagnostic)
//!  * `delete p` when `Deleted`→ **provable double-free** diagnostic
//!  * `delete p` when `OwnedScope` → **scope-delete double-free** diagnostic
//!  * `p = <new T()>`          → re-`Owned` (the lattice resets)
//!  * `p = <anything else>`    → **untracked** (conservative)
//!  * any other use of `p` (arg pass, return, member access, address-of,
//!    capture, store, …) → **untracked** (conservative — a later `delete` then
//!    can't false-positive, and a move can't be claimed as a double free)
//!
//! ## Control flow (the conservative join)
//!
//! Branches are walked with a **clone** of the entry state so an in-branch
//! `delete; delete` is still caught, then the branch-end states are **merged
//! conservatively**: a binding keeps its state only if **every** path agrees on
//! it; any disagreement (or a state changed on only some paths) drops it to
//! **untracked**. Loops are walked once (catching an in-iteration
//! `delete; delete`) and then merged the same way, so a delete that is only a
//! double-free across iterations — which is not statically provable — is never
//! claimed. The rule is **zero false positives**: when unsure, stop tracking.

use std::collections::HashMap;

use newbf_parser::{AssignOp, Expr, Item, Member, MethodBody, PrefixKw, Stmt, TypeDecl};

use crate::Diagnostic;
use crate::build::SourceFile;
use crate::intern::Interner;
use crate::model::{DefGraph, TypeKindD};

/// The ownership state of one tracked local binding.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    /// Initialized from a user-written `new ClassType()` and still owned.
    Owned,
    /// Initialized from a `scope ClassType()` — its lifetime is the enclosing
    /// scope, so an explicit `delete` of it is a double free.
    OwnedScope,
    /// `delete`d once; a second `delete` (no intervening reassignment) is a
    /// provable double free.
    Deleted,
}

/// MS-T5 entry point: walk every method/ctor/dtor/property-accessor body in the
/// user `files` and return the provable double-free / scope-delete diagnostics.
/// Pure sema — no IR, no LLVM. Never panics on a partial AST.
pub(crate) fn check_delete_flow(
    files: &[SourceFile<'_>],
    graph: &DefGraph,
    interner: &Interner,
) -> Vec<Diagnostic> {
    let classes = class_name_set(graph, interner);
    let mut diags = Vec::new();
    for f in files {
        walk_items(&f.unit.items, f.src, &classes, &mut diags);
    }
    diags
}

/// The simple names of every user-declared **class** in the def graph. A
/// `new T()` / `scope T()` whose constructed type's simple name is in this set
/// is an owning-class allocation (the operand may carry generic args —
/// `new List<int32>()` — which we strip to the base name `List`).
///
/// Only `TypeKindD::Class` qualifies: structs are value types (no heap owner),
/// interfaces/enums/delegates/aliases are not constructed-and-owned here.
fn class_name_set(graph: &DefGraph, interner: &Interner) -> std::collections::HashSet<String> {
    let mut set = std::collections::HashSet::new();
    for t in &graph.types {
        if t.kind == TypeKindD::Class {
            set.insert(interner.resolve(t.name).to_string());
        }
    }
    set
}

// ── item / member traversal ──────────────────────────────────────────────────

fn walk_items(
    items: &[Item],
    src: &str,
    classes: &std::collections::HashSet<String>,
    diags: &mut Vec<Diagnostic>,
) {
    for it in items {
        match it {
            Item::Namespace { body: Some(b), .. } => walk_items(b, src, classes, diags),
            Item::Type(td) => walk_type(td, src, classes, diags),
            _ => {}
        }
    }
}

fn walk_type(
    td: &TypeDecl,
    src: &str,
    classes: &std::collections::HashSet<String>,
    diags: &mut Vec<Diagnostic>,
) {
    for m in &td.members {
        match m {
            Member::Method { body, .. }
            | Member::Constructor { body, .. }
            | Member::Destructor { body, .. } => walk_body(body, src, classes, diags),
            Member::Property { accessors, .. } => {
                for a in accessors {
                    walk_body(&a.body, src, classes, diags);
                }
            }
            Member::Nested(n) => walk_type(n, src, classes, diags),
            _ => {}
        }
    }
}

fn walk_body(
    body: &MethodBody,
    src: &str,
    classes: &std::collections::HashSet<String>,
    diags: &mut Vec<Diagnostic>,
) {
    // Only block bodies can contain `let p = …; delete p;` sequences. An
    // expression body (`=> e`) has no locals to track.
    if let MethodBody::Block(s) = body {
        let mut env: Env = HashMap::new();
        let mut cx = Cx { src, classes, diags };
        cx.walk_stmt(s, &mut env);
    }
}

/// The per-body binding-state map: binding name → its current lattice state.
/// Untracked bindings are simply absent. (The diagnostic location is taken from
/// the offending `delete` statement's own span, so no per-binding span is kept.)
type Env = HashMap<String, State>;

struct Cx<'a> {
    src: &'a str,
    classes: &'a std::collections::HashSet<String>,
    diags: &'a mut Vec<Diagnostic>,
}

impl Cx<'_> {
    /// Walk one statement, mutating `env` in place for straight-line flow and
    /// recursing with cloned/merged states across control flow.
    fn walk_stmt(&mut self, s: &Stmt, env: &mut Env) {
        match s {
            Stmt::Block { stmts, .. } => {
                for st in stmts {
                    self.walk_stmt(st, env);
                }
            }
            Stmt::Expr { expr, .. } => self.walk_expr(expr, env),
            Stmt::Local {
                name, init, ..
            } => {
                let bind = name.text(self.src).to_string();
                // A new declaration shadows any previous tracking of this name.
                env.remove(&bind);
                if let Some(init) = init {
                    // The initializer is an arbitrary expression: first let it
                    // taint any *other* tracked binding it uses (e.g.
                    // `let q = Wrap(p);` moves `p`), then classify it for `bind`.
                    self.walk_expr_uses(init, env, Some(&bind));
                    match self.alloc_kind(init) {
                        AllocClass::Owned => {
                            env.insert(bind, State::Owned);
                        }
                        AllocClass::Scope => {
                            env.insert(bind, State::OwnedScope);
                        }
                        AllocClass::Other => {}
                    }
                }
            }
            Stmt::Locals { decls, .. } => {
                for d in decls {
                    self.walk_stmt(d, env);
                }
            }
            // `delete p` as a statement-level expression is handled by walk_expr,
            // but assignments reach here via Stmt::Expr too. Control flow:
            Stmt::If { cond, then, els, .. } => {
                self.walk_expr(cond, env);
                let mut then_env = env.clone();
                self.walk_stmt(then, &mut then_env);
                let mut else_env = env.clone();
                if let Some(e) = els {
                    self.walk_stmt(e, &mut else_env);
                }
                *env = merge(&then_env, &else_env);
            }
            Stmt::While { cond, body, .. } => {
                self.walk_expr(cond, env);
                self.walk_loop(body, env);
            }
            Stmt::DoWhile { body, cond, .. } => {
                self.walk_loop(body, env);
                self.walk_expr(cond, env);
            }
            Stmt::For {
                init,
                init_extra,
                cond,
                update,
                update_extra,
                body,
                ..
            } => {
                if let Some(i) = init {
                    self.walk_stmt(i, env);
                }
                for e in init_extra {
                    self.walk_stmt(e, env);
                }
                if let Some(c) = cond {
                    self.walk_expr(c, env);
                }
                // The body + updates re-run an unknown number of times → loop join.
                let mut body_env = env.clone();
                self.walk_stmt(body, &mut body_env);
                for u in update_extra {
                    self.walk_expr(u, &mut body_env);
                }
                if let Some(u) = update {
                    self.walk_expr(u, &mut body_env);
                }
                *env = merge(env, &body_env);
            }
            Stmt::ForEach { iter, body, .. } => {
                self.walk_expr(iter, env);
                self.walk_loop(body, env);
            }
            Stmt::Switch { scrutinee, arms, .. } => {
                self.walk_expr(scrutinee, env);
                // Each arm is an independent path; merge all arm-ends with the
                // pre-switch state (fallthrough = the no-arm path).
                let mut merged = env.clone();
                for arm in arms {
                    let mut arm_env = env.clone();
                    if let Some(p) = &arm.pattern {
                        self.walk_expr(p, &mut arm_env);
                    }
                    for e in &arm.extra {
                        self.walk_expr(e, &mut arm_env);
                    }
                    if let Some(g) = &arm.guard {
                        self.walk_expr(g, &mut arm_env);
                    }
                    for st in &arm.body {
                        self.walk_stmt(st, &mut arm_env);
                    }
                    merged = merge(&merged, &arm_env);
                }
                *env = merged;
            }
            // `return p` moves `p` (it escapes) — untrack every binding the
            // returned expression uses.
            Stmt::Return { value: Some(v), .. } => self.walk_expr_uses(v, env, None),
            Stmt::Defer { body, .. } => {
                // A deferred body runs at scope exit on an unknown ordering;
                // walk it for in-body double-frees, then merge conservatively.
                let mut d_env = env.clone();
                self.walk_stmt(body, &mut d_env);
                *env = merge(env, &d_env);
            }
            Stmt::LocalFunction { body, .. } => {
                // A nested function has its own locals; analyse it in isolation
                // (it can't safely share the enclosing binding map).
                let mut inner: Env = HashMap::new();
                self.walk_stmt(body, &mut inner);
            }
            // Break/continue carry no tracked operand. Empty/error/mixin: nothing.
            _ => {}
        }
    }

    /// A loop body that runs an unknown number of times: walk it once (to catch
    /// an in-iteration `delete; delete`) then merge with the entry state so any
    /// binding it deleted/reassigned/moved becomes untracked afterward (a
    /// cross-iteration double free is not statically provable → never claimed).
    fn walk_loop(&mut self, body: &Stmt, env: &mut Env) {
        let mut body_env = env.clone();
        self.walk_stmt(body, &mut body_env);
        *env = merge(env, &body_env);
    }

    /// Walk an expression in *statement* position, recognising the two
    /// state-changing shapes — `delete p` and `p = <expr>` — and otherwise
    /// treating every binding it mentions as conservatively used (untracked).
    fn walk_expr(&mut self, e: &Expr, env: &mut Env) {
        match e {
            // `delete p` where `p` is a bare tracked binding: the lattice step.
            Expr::Prefix { kw: PrefixKw::Delete, operand, span, .. } => {
                if let Expr::Ident(s) = strip_paren(operand) {
                    let bind = s.text(self.src).to_string();
                    if let Some(st) = env.get(&bind).copied() {
                        match st {
                            State::Owned => {
                                env.insert(bind, State::Deleted);
                            }
                            State::Deleted => {
                                self.diags.push(Diagnostic {
                                    span: *span,
                                    message: format!(
                                        "provable double-free: '{bind}' is deleted again with no \
                                         intervening reassignment"
                                    ),
                                });
                                // Stay Deleted: a third delete is also a double free.
                            }
                            State::OwnedScope => {
                                self.diags.push(Diagnostic {
                                    span: *span,
                                    message: format!(
                                        "provable double-free: 'delete {bind}' frees a \
                                         scope-allocated object that the scope cleanup also frees"
                                    ),
                                });
                                // Now also Deleted (the explicit delete happened);
                                // a further delete is a plain double free too.
                                env.insert(bind, State::Deleted);
                            }
                        }
                        return;
                    }
                    // Untracked binding: nothing to say. Still fall through to
                    // taint nothing (a bare ident has no other uses).
                    return;
                }
                // `delete <non-ident>` (e.g. `delete p.child`, `delete arr[i]`):
                // any tracked binding inside is conservatively used.
                self.walk_expr_uses(operand, env, None);
            }
            // `p = <expr>` — reassignment resets the lattice for `p`.
            Expr::Assign { op: AssignOp::Assign, target, value, .. } => {
                // The RHS may use other tracked bindings (move/alias them).
                self.walk_expr_uses(value, env, None);
                if let Expr::Ident(s) = strip_paren(target) {
                    let bind = s.text(self.src).to_string();
                    // Reset: a `new T()` re-owns; anything else untracks.
                    match self.alloc_kind(value) {
                        AllocClass::Owned => {
                            env.insert(bind, State::Owned);
                        }
                        AllocClass::Scope => {
                            env.insert(bind, State::OwnedScope);
                        }
                        AllocClass::Other => {
                            env.remove(&bind);
                        }
                    }
                } else {
                    // `p.x = …` / `arr[i] = …`: the target's bindings are used.
                    self.walk_expr_uses(target, env, None);
                }
            }
            // Any other expression: every tracked binding it mentions is
            // conservatively *used* → untracked (a move/alias we can't follow).
            _ => self.walk_expr_uses(e, env, None),
        }
    }

    /// Recursively untrack every tracked binding that `e` *uses* in a way the
    /// analysis cannot follow (argument pass, return, member/index base,
    /// address-of, capture, …). `skip` names the binding currently being
    /// declared (so `let q = q;`-style self-reference, which can't happen for a
    /// fresh `new`, doesn't matter, but we still avoid untracking it spuriously).
    ///
    /// This is the conservatism workhorse: the *moment* a tracked binding flows
    /// anywhere other than a bare `delete p` / `p = …` target, we stop tracking
    /// it — guaranteeing a later `delete` can never produce a false positive.
    fn walk_expr_uses(&mut self, e: &Expr, env: &mut Env, skip: Option<&str>) {
        match e {
            Expr::Ident(s) => {
                let name = s.text(self.src);
                if Some(name) != skip {
                    env.remove(name);
                }
            }
            Expr::Paren { inner, .. } => self.walk_expr_uses(inner, env, skip),
            Expr::Unary { operand, .. }
            | Expr::PostInc { operand, .. }
            | Expr::PostDec { operand, .. } => self.walk_expr_uses(operand, env, skip),
            Expr::Binary { lhs, rhs, .. } => {
                self.walk_expr_uses(lhs, env, skip);
                self.walk_expr_uses(rhs, env, skip);
            }
            Expr::Assign { target, value, .. } => {
                self.walk_expr_uses(target, env, skip);
                self.walk_expr_uses(value, env, skip);
            }
            Expr::Ternary { cond, then, els, .. } => {
                self.walk_expr_uses(cond, env, skip);
                self.walk_expr_uses(then, env, skip);
                self.walk_expr_uses(els, env, skip);
            }
            Expr::Call { callee, args, .. } => {
                self.walk_expr_uses(callee, env, skip);
                for a in args {
                    self.walk_expr_uses(a, env, skip);
                }
            }
            Expr::MixinCall { callee, args, .. } => {
                self.walk_expr_uses(callee, env, skip);
                for a in args {
                    self.walk_expr_uses(a, env, skip);
                }
            }
            Expr::Index { base, args, .. } => {
                self.walk_expr_uses(base, env, skip);
                for a in args {
                    self.walk_expr_uses(a, env, skip);
                }
            }
            Expr::Member { base, .. } => self.walk_expr_uses(base, env, skip),
            Expr::Prefix { operand, .. } => self.walk_expr_uses(operand, env, skip),
            Expr::Generic { base, .. } => self.walk_expr_uses(base, env, skip),
            Expr::Cast { operand, .. } => self.walk_expr_uses(operand, env, skip),
            Expr::DotIdent { .. } => {}
            Expr::Tuple { elems, .. } => {
                for el in elems {
                    self.walk_expr_uses(el, env, skip);
                }
            }
            Expr::Initializer { base, entries, .. } => {
                self.walk_expr_uses(base, env, skip);
                for en in entries {
                    self.walk_expr_uses(en, env, skip);
                }
            }
            Expr::Named { value, .. } => self.walk_expr_uses(value, env, skip),
            Expr::Interp { parts, .. } => {
                for p in parts {
                    if let newbf_parser::InterpPart::Hole(h) = p {
                        self.walk_expr_uses(h, env, skip);
                    }
                }
            }
            Expr::Lambda { body, .. } => {
                // A lambda may capture a tracked binding — un-followable → drop
                // every binding the body mentions. Use a throwaway inner walk:
                // it can only *use* (capture) outer bindings, never re-own them.
                self.untrack_lambda(body, env);
            }
            // Literals / this / base / error / sizeof / typeof: no bindings.
            _ => {}
        }
    }

    /// A lambda body can only capture (use) outer bindings — drop every binding
    /// it mentions so a captured owner is no longer tracked (conservative).
    fn untrack_lambda(&mut self, s: &Stmt, env: &mut Env) {
        match s {
            Stmt::Block { stmts, .. } => {
                for st in stmts {
                    self.untrack_lambda(st, env);
                }
            }
            Stmt::Expr { expr, .. } => self.walk_expr_uses(expr, env, None),
            Stmt::Return { value: Some(v), .. } => self.walk_expr_uses(v, env, None),
            Stmt::Local { init: Some(i), .. } => self.walk_expr_uses(i, env, None),
            Stmt::If { cond, then, els, .. } => {
                self.walk_expr_uses(cond, env, None);
                self.untrack_lambda(then, env);
                if let Some(e) = els {
                    self.untrack_lambda(e, env);
                }
            }
            Stmt::While { cond, body, .. } => {
                self.walk_expr_uses(cond, env, None);
                self.untrack_lambda(body, env);
            }
            Stmt::For { body, .. } | Stmt::ForEach { body, .. } | Stmt::Defer { body, .. } => {
                self.untrack_lambda(body, env);
            }
            _ => {}
        }
    }

    /// Classify an initializer/RHS expression: is it a user-written `new`/`scope`
    /// of an owning **class**? (memory-safety.md §B1 — only these are tracked).
    fn alloc_kind(&self, e: &Expr) -> AllocClass {
        match strip_paren(e) {
            Expr::Prefix { kw, operand, .. } if matches!(kw, PrefixKw::New | PrefixKw::Scope) => {
                // Array `new T[n]` / array-init / object-initializer / String:
                // only a bare-class construction `new T(...)` (optionally generic)
                // counts. `ctor_class_name` returns the simple type name for the
                // construction shapes and `None` for array shapes.
                if is_array_new(operand) {
                    return AllocClass::Other;
                }
                let base = ctor_class_name(operand, self.src);
                match base {
                    Some(name) if self.classes.contains(name) => {
                        if *kw == PrefixKw::Scope {
                            AllocClass::Scope
                        } else {
                            AllocClass::Owned
                        }
                    }
                    _ => AllocClass::Other,
                }
            }
            _ => AllocClass::Other,
        }
    }
}

/// The classification of an initializer for tracking purposes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AllocClass {
    /// `new ClassType(...)` — track as `Owned`.
    Owned,
    /// `scope ClassType(...)` — track as `OwnedScope`.
    Scope,
    /// Anything else (array, String, collection literal, value type, call, …):
    /// not tracked.
    Other,
}

/// Conservative join of two branch-end binding maps: a binding survives with a
/// state only if **both** sides agree on it; any disagreement (present-vs-absent
/// or differing state) drops it (it becomes untracked). This guarantees a
/// `delete` after a merge is flagged only when the binding is provably in that
/// state on *every* incoming path — the zero-false-positive rule.
fn merge(a: &Env, b: &Env) -> Env {
    let mut out = HashMap::new();
    for (k, va) in a {
        if let Some(vb) = b.get(k)
            && va == vb
        {
            out.insert(k.clone(), *va);
        }
    }
    out
}

// ── small AST helpers (mirroring lower.rs, but text-only) ─────────────────────

/// Peel `( … )` wrappers off an expression.
fn strip_paren(e: &Expr) -> &Expr {
    match e {
        Expr::Paren { inner, .. } => strip_paren(inner),
        _ => e,
    }
}

/// Whether a `new`/`scope` operand is an **array** allocation (`new T[n]`,
/// `new T[n]{…}`, `new T[](…)`) rather than an object construction. Array
/// allocations are never tracked (they aren't owning classes — §B1).
fn is_array_new(operand: &Expr) -> bool {
    match strip_paren(operand) {
        // `new T[n]` — an index whose base is a type name.
        Expr::Index { .. } => true,
        // `new T[n] { … }` — initializer wrapping an index base.
        Expr::Initializer { base, .. } => matches!(strip_paren(base), Expr::Index { .. }),
        // `new T[](…)` — a call whose callee is an index shape.
        Expr::Call { callee, .. } => matches!(strip_paren(callee), Expr::Index { .. }),
        _ => false,
    }
}

/// The simple (last-segment) class name a `new`/`scope` operand constructs:
/// `new C(a)` → `C`, `new List<int32>()` → `List`, `new C` → `C`. Returns
/// `None` for array/initializer/non-construction shapes. Mirrors lower.rs's
/// `ctor_class_name` + `generic_new_parts`, but works purely off source text.
fn ctor_class_name<'s>(e: &Expr, src: &'s str) -> Option<&'s str> {
    match strip_paren(e) {
        Expr::Ident(s) => Some(s.text(src)),
        Expr::Call { callee, .. } => ctor_class_name(callee, src),
        Expr::Generic { base, .. } => ctor_class_name(base, src),
        // `new T() { field = v }` — the construction is the initializer's base.
        Expr::Initializer { base, .. } => ctor_class_name(base, src),
        _ => None,
    }
}
