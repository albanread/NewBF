//! MS-T5/MS-T6 — compile-time delete-flow: provable double-free **and**
//! provable-leak analysis (Track B).
//!
//! A **pure-sema** pass (no IR, no LLVM) that tracks user-written
//! `new ClassType()` locals through an ownership lattice and diagnoses three
//! kinds of *provable* mistake:
//!
//!  * a **double-`delete`** of the same binding with no intervening
//!    reassignment (MS-T5),
//!  * a **`delete` of a `scope`-bound binding** (the `scope` lifetime cleanup
//!    will also free it — a guaranteed double free) (MS-T5), and
//!  * a **leak** — a binding still `Owned` at a function-body exit edge: it was
//!    `new`'d and never deleted, moved, dropped, or `scope`-bound (MS-T6).
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
//! ## The lattice (§B2 — the full Owned/Moved/Dropped transitions)
//!
//! Per tracked binding ∈ `{Owned, OwnedScope, Deleted, Moved, Dropped}` (an
//! untracked binding is simply absent from the map):
//!
//!  * `let p = new T()`        → `Owned`
//!  * `let p = scope T()`      → `OwnedScope` (never a leak — auto-freed)
//!  * `delete p` when `Owned`  → `Deleted` (no diagnostic)
//!  * `delete p` when `Deleted`→ **provable double-free** diagnostic
//!  * `delete p` when `OwnedScope` → **scope-delete double-free** diagnostic
//!  * `p = <new T()>`          → re-`Owned` (the lattice resets)
//!  * `p = <anything else>`    → **untracked** (conservative)
//!  * **arg-pass `f(p)` / method call `p.M(…)` / member-read `p.f` / index-read
//!    `p[i]` / `p` in a comparison** → **stays `Owned`** (Beef passes by
//!    reference; the callee/reader does not take ownership). This is the MS-T6
//!    refinement of MS-T5's blanket untrack — it keeps leak detection alive for
//!    `list_hof`/`prelude_probe`-shaped code while staying sound.
//!  * **`return p`** → `Moved` (ownership leaves; not a leak)
//!  * **tracked-reassignment `q = p` / `let q = p`** (RHS is a bare tracked
//!    binding) → `p` becomes `Moved` (it is aliased out)
//!  * **capture by a lambda / field-store `this.f = p` / `obj.f = p` /
//!    address-of `&p` / `ref p` / `out p` / store into an aggregate** →
//!    `Dropped` (ownership escapes to a place the analysis cannot follow;
//!    conservatively **not** a leak — never diagnosed)
//!
//! ## The leak rule + the exit-edge survivor scan (MS-T6)
//!
//! At **every** body exit edge — the fall-through end of the body and each
//! `return` — any binding still `Owned` (NOT `OwnedScope`, `Deleted`, `Moved`,
//! `Dropped`, or absent) is a **provable leak**. The scan runs on the env *as it
//! stands at that edge* (for a `return p`, after `p` has moved), so the returned
//! value is never itself flagged, while a sibling `new` that was never freed is.
//!
//! ## Control flow (the conservative join)
//!
//! Branches are walked with a **clone** of the entry state so an in-branch
//! `delete; delete` is still caught, then the branch-end states are **merged
//! conservatively**: a binding keeps its state only if **every** path agrees on
//! it; any disagreement (or a state changed on only some paths) drops it to
//! **untracked** (`Dropped` effectively wins any merge — an untracked binding is
//! never diagnosed). Loops are walked once (catching an in-iteration
//! `delete; delete`) and then merged the same way, so a delete/leak that is only
//! provable across iterations is never claimed. The rule is **zero false
//! positives**: when unsure, stop tracking — never diagnose.

use std::collections::HashMap;

use newbf_lexer::Span;
use newbf_parser::{AssignOp, Expr, Item, Member, MethodBody, PrefixKw, Stmt, TypeDecl, UnOp};

use crate::Diagnostic;
use crate::build::SourceFile;
use crate::intern::Interner;
use crate::model::{DefGraph, TypeKindD};

/// The ownership state of one tracked local binding.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    /// Initialized from a user-written `new ClassType()` and still owned —
    /// surviving to an exit edge in this state is a **provable leak** (MS-T6).
    Owned,
    /// Initialized from a `scope ClassType()` — its lifetime is the enclosing
    /// scope, so an explicit `delete` of it is a double free, and it is **never
    /// a leak** (the scope cleanup auto-frees it).
    OwnedScope,
    /// `delete`d once; a second `delete` (no intervening reassignment) is a
    /// provable double free. Not a leak.
    Deleted,
    /// Ownership left via `return p` or `q = p` (tracked-binding alias). Not a
    /// leak (the value escaped to a caller/alias), and a later `delete` would be
    /// on the *new* owner, so this binding is no longer double-free-tracked.
    Moved,
    /// Ownership escaped un-followably (captured by a lambda, stored into a
    /// field/aggregate, address-taken, `ref`/`out`-passed). Conservatively
    /// **not** a leak (we cannot prove it is never freed) — never diagnosed.
    Dropped,
}

/// Entry point: walk every method/ctor/dtor/property-accessor body in the user
/// `files` and return the provable double-free / scope-delete / leak
/// diagnostics. Pure sema — no IR, no LLVM. Never panics on a partial AST.
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
        let mut cx = Cx {
            src,
            classes,
            diags,
            locals: std::collections::HashSet::new(),
        };
        // The fall-through end of the body is an exit edge: any binding still
        // `Owned` here was `new`'d and never freed/moved/dropped → leak (MS-T6).
        // Only scan it when control can actually reach the body end — a body that
        // always `return`s already scanned at the `return` (avoids double-report).
        if cx.walk_stmt(s, &mut env) {
            cx.exit_scan(&env);
        }
    }
}

/// The per-body binding-state map: binding name → its current lattice state.
/// Untracked bindings are simply absent. A per-binding span is kept so the leak
/// diagnostic can point at the originating `new` site (the double-free
/// diagnostic uses the offending `delete`'s own span).
type Env = HashMap<String, Tracked>;

/// A tracked binding: its lattice state plus the span of the `new` that
/// introduced it (used to locate the leak diagnostic).
#[derive(Clone, Copy, Debug)]
struct Tracked {
    state: State,
    /// Span of the originating `new ClassType()` allocation.
    new_span: Span,
}

struct Cx<'a> {
    src: &'a str,
    classes: &'a std::collections::HashSet<String>,
    diags: &'a mut Vec<Diagnostic>,
    /// Names declared as **locals** in the current body (via `Stmt::Local`).
    /// Used to distinguish `local = new T()` (a trackable re-own) from
    /// `someField = new T()` (a field-store — the object's ownership escapes to
    /// the field, NOT a leak). A bare-ident assignment target that is **not** a
    /// declared local is conservatively treated as a field/aggregate store.
    locals: std::collections::HashSet<String>,
}

impl Cx<'_> {
    /// The exit-edge survivor scan (MS-T6): every binding still `Owned` at an
    /// exit edge is a provable leak. Run at the fall-through body end and at each
    /// `return` (on the env *after* the returned value's move has been applied).
    fn exit_scan(&mut self, env: &Env) {
        // Deterministic order so the diagnostic list is stable across runs.
        let mut leaks: Vec<(Span, &str)> = env
            .iter()
            .filter(|(_, t)| t.state == State::Owned)
            .map(|(name, t)| (t.new_span, name.as_str()))
            .collect();
        leaks.sort_by_key(|(s, _)| (s.lo, s.hi));
        for (new_span, name) in leaks {
            self.diags.push(Diagnostic {
                span: new_span,
                message: format!(
                    "provable leak: '{name}' is allocated with `new` but never deleted, \
                     moved, or scope-bound on the path to this exit"
                ),
            });
        }
    }

    /// Walk one statement, mutating `env` in place for straight-line flow and
    /// recursing with cloned/merged states across control flow. Returns `true`
    /// if control can **fall through** past this statement, `false` if it always
    /// diverges (`return`/`break`/`continue`, or a block whose last reachable
    /// statement diverges). Divergence drives the exit-edge scan: a body that
    /// always `return`s is scanned at the `return`, not again at the body end.
    fn walk_stmt(&mut self, s: &Stmt, env: &mut Env) -> bool {
        match s {
            Stmt::Block { stmts, .. } => {
                // Walk until a statement diverges; statements after it are dead.
                for st in stmts {
                    if !self.walk_stmt(st, env) {
                        return false;
                    }
                }
                true
            }
            Stmt::Expr { expr, .. } => {
                self.walk_expr(expr, env);
                true
            }
            Stmt::Local { name, init, span, .. } => {
                let bind = name.text(self.src).to_string();
                // Record it as a declared local (so a later `bind = new T()` is a
                // trackable re-own, not a field-store).
                self.locals.insert(bind.clone());
                // A new declaration shadows any previous tracking of this name.
                env.remove(&bind);
                if let Some(init) = init {
                    // The initializer is an arbitrary expression: first let it
                    // flow any *other* tracked binding it uses (e.g.
                    // `let q = p;` moves `p`), then classify it for `bind`.
                    self.flow_init(init, env);
                    match self.alloc_kind(init) {
                        AllocClass::Owned => {
                            env.insert(bind, Tracked { state: State::Owned, new_span: *span });
                        }
                        AllocClass::Scope => {
                            env.insert(
                                bind,
                                Tracked { state: State::OwnedScope, new_span: *span },
                            );
                        }
                        AllocClass::Other => {}
                    }
                }
                true
            }
            Stmt::Locals { decls, .. } => {
                for d in decls {
                    self.walk_stmt(d, env);
                }
                true
            }
            Stmt::If { cond, then, els, .. } => {
                self.flow_use(cond, env);
                let mut then_env = env.clone();
                let then_ft = self.walk_stmt(then, &mut then_env);
                let mut else_env = env.clone();
                let else_ft = match els {
                    Some(e) => self.walk_stmt(e, &mut else_env),
                    // No `else` → the "else" path falls through with the pre-`if`
                    // state unchanged.
                    None => true,
                };
                // The post-`if` state reflects only the paths that fall through.
                // A diverging branch (it `return`ed/`break`ed) does not
                // contribute its end-state — this both avoids losing precision
                // and keeps the merge sound.
                *env = match (then_ft, else_ft) {
                    (true, true) => merge(&then_env, &else_env),
                    (true, false) => then_env,
                    (false, true) => else_env,
                    // Both diverge → code after the `if` is unreachable; the env
                    // value is moot. Keep a conservative merge.
                    (false, false) => merge(&then_env, &else_env),
                };
                then_ft || else_ft
            }
            Stmt::While { cond, body, .. } => {
                self.flow_use(cond, env);
                self.walk_loop(body, env);
                // A `while` may execute zero times → control always falls through.
                true
            }
            Stmt::DoWhile { body, cond, .. } => {
                self.walk_loop(body, env);
                self.flow_use(cond, env);
                true
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
                    self.flow_use(c, env);
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
                // A `for` may execute zero times → control always falls through.
                true
            }
            Stmt::ForEach { iter, body, .. } => {
                self.flow_use(iter, env);
                self.walk_loop(body, env);
                true
            }
            Stmt::Switch { scrutinee, arms, .. } => {
                self.flow_use(scrutinee, env);
                // Each arm is an independent path; merge the fall-through arm-ends
                // with the pre-switch state (fallthrough = the no-arm path).
                let mut merged = env.clone();
                for arm in arms {
                    let mut arm_env = env.clone();
                    if let Some(p) = &arm.pattern {
                        self.flow_use(p, &mut arm_env);
                    }
                    for e in &arm.extra {
                        self.flow_use(e, &mut arm_env);
                    }
                    if let Some(g) = &arm.guard {
                        self.flow_use(g, &mut arm_env);
                    }
                    let mut arm_ft = true;
                    for st in &arm.body {
                        if !self.walk_stmt(st, &mut arm_env) {
                            arm_ft = false;
                            break;
                        }
                    }
                    // A diverging arm (it `return`ed) does not contribute its
                    // end-state to the post-switch merge.
                    if arm_ft {
                        merged = merge(&merged, &arm_env);
                    }
                }
                *env = merged;
                true
            }
            // `return p` moves `p` (it escapes via the return value); any other
            // binding the returned expression *uses* keeps its state. Then this
            // is an exit edge → scan the post-move env for `Owned` survivors.
            // Control diverges (does not fall through).
            Stmt::Return { value, .. } => {
                if let Some(v) = value {
                    self.flow_return(v, env);
                }
                self.exit_scan(env);
                false
            }
            // `break`/`continue` divert control out of / back to the loop head —
            // they do not fall through to the next statement. (They are not body
            // exit edges, so no leak scan: a binding live across a loop iteration
            // is handled by the loop merge.)
            Stmt::Break { .. } | Stmt::Continue { .. } => false,
            Stmt::Defer { body, .. } => {
                // A deferred body runs at scope exit on an unknown ordering;
                // walk it for in-body double-frees, then merge conservatively.
                let mut d_env = env.clone();
                self.walk_stmt(body, &mut d_env);
                *env = merge(env, &d_env);
                true
            }
            Stmt::LocalFunction { body, .. } => {
                // A nested function has its own locals; analyse it in isolation
                // (it can't safely share the enclosing binding map). Run its own
                // exit-scan so a leak inside the nested fn is still caught. Save +
                // restore the declared-locals set so the inner fn's locals don't
                // leak into the outer body's local/field disambiguation.
                let saved = std::mem::take(&mut self.locals);
                let mut inner: Env = HashMap::new();
                if self.walk_stmt(body, &mut inner) {
                    self.exit_scan(&inner);
                }
                self.locals = saved;
                // The declaration itself falls through (defining a local fn does
                // not transfer control).
                true
            }
            // Empty/error/mixin-decl: no flow effect, falls through.
            _ => true,
        }
    }

    /// A loop body that runs an unknown number of times: walk it once (to catch
    /// an in-iteration `delete; delete`) then merge with the entry state so any
    /// binding it deleted/reassigned/moved becomes untracked afterward (a
    /// cross-iteration double free / leak is not statically provable → never
    /// claimed).
    fn walk_loop(&mut self, body: &Stmt, env: &mut Env) {
        let mut body_env = env.clone();
        self.walk_stmt(body, &mut body_env);
        *env = merge(env, &body_env);
    }

    /// Walk an expression in *statement* position, recognising the two
    /// state-changing shapes — `delete p` and `p = <expr>` — and otherwise
    /// flowing every binding it mentions through the use rules.
    fn walk_expr(&mut self, e: &Expr, env: &mut Env) {
        match e {
            // `delete p` where `p` is a bare tracked binding: the lattice step.
            Expr::Prefix { kw: PrefixKw::Delete, operand, span, .. } => {
                if let Expr::Ident(s) = strip_paren(operand) {
                    let bind = s.text(self.src).to_string();
                    if let Some(t) = env.get(&bind).copied() {
                        match t.state {
                            State::Owned => {
                                env.insert(bind, Tracked { state: State::Deleted, ..t });
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
                                env.insert(bind, Tracked { state: State::Deleted, ..t });
                            }
                            // Moved/Dropped: ownership already left — a `delete`
                            // here is on a value we no longer track; stay silent
                            // (conservative — no provable double free).
                            State::Moved | State::Dropped => {}
                        }
                        return;
                    }
                    // Untracked binding: nothing to say.
                    return;
                }
                // `delete <non-ident>` (e.g. `delete p.child`, `delete arr[i]`):
                // any tracked binding inside is conservatively used.
                self.flow_use(operand, env);
            }
            // `p = <expr>` — reassignment resets the lattice for `p`.
            Expr::Assign { op: AssignOp::Assign, target, value, .. } => {
                let bare_local = match strip_paren(target) {
                    Expr::Ident(s) => {
                        let name = s.text(self.src);
                        // Only a **declared local** is a trackable re-own target.
                        // A bare ident that is NOT a local is a field (`s_cached =
                        // new T()`) or an outer name — assigning `new` there is a
                        // field-store (ownership escapes to the field → NOT a leak),
                        // so treat it like `this.f = new T()`.
                        self.locals.contains(name).then(|| name.to_string())
                    }
                    _ => None,
                };
                if let Some(bind) = bare_local {
                    // First flow the RHS: if it aliases another tracked binding
                    // (`p = q`), that binding is moved out. Use the *target* name
                    // as the new owner hint so a self-assign `p = p` is a no-op.
                    self.flow_assign_rhs(value, env, Some(&bind));
                    // Reset the target: a `new T()` re-owns; anything else untracks.
                    match self.alloc_kind(value) {
                        AllocClass::Owned => {
                            env.insert(
                                bind,
                                Tracked { state: State::Owned, new_span: value.span() },
                            );
                        }
                        AllocClass::Scope => {
                            env.insert(
                                bind,
                                Tracked { state: State::OwnedScope, new_span: value.span() },
                            );
                        }
                        AllocClass::Other => {
                            env.remove(&bind);
                        }
                    }
                } else {
                    // `p.x = q` / `arr[i] = q` / `someField = q`: a field/aggregate
                    // store. The *value* (if a bare tracked binding) escapes
                    // un-followably → Dropped; the target's bindings are plain uses.
                    // (A bare-ident non-local target carries no tracked binding to
                    // flow, so flowing it is a harmless no-op.)
                    self.flow_store_value(value, env);
                    self.flow_use(target, env);
                }
            }
            // Any other expression in statement position: flow every tracked
            // binding it mentions through the use rules (calls/method-calls/
            // member-reads keep `Owned`; lambdas/address-of drop).
            _ => self.flow_use(e, env),
        }
    }

    // ── flow helpers: apply the right disposition per syntactic position ──────

    /// Flow a local-initializer RHS (`let q = <init>`). If it is a bare tracked
    /// binding `let q = p`, `p` is aliased out → `Moved`; otherwise treat it as a
    /// plain use.
    fn flow_init(&mut self, e: &Expr, env: &mut Env) {
        if let Expr::Ident(s) = strip_paren(e) {
            let name = s.text(self.src);
            self.set_moved(name, env);
            return;
        }
        self.flow_use(e, env);
    }

    /// Flow an assignment RHS `p = <value>` where `p` is `owner` (the target).
    /// A bare tracked binding on the RHS is aliased out → `Moved` (unless it is
    /// the owner itself, a self-assign).
    fn flow_assign_rhs(&mut self, e: &Expr, env: &mut Env, owner: Option<&str>) {
        if let Expr::Ident(s) = strip_paren(e) {
            let name = s.text(self.src);
            if Some(name) != owner {
                self.set_moved(name, env);
            }
            return;
        }
        self.flow_use(e, env);
    }

    /// Flow a `return <value>`: a bare tracked binding is moved out (ownership
    /// escapes to the caller). Anything else is a plain use.
    fn flow_return(&mut self, e: &Expr, env: &mut Env) {
        if let Expr::Ident(s) = strip_paren(e) {
            let name = s.text(self.src);
            self.set_moved(name, env);
            return;
        }
        self.flow_use(e, env);
    }

    /// Flow the *value* of a field/aggregate store `lhs = <value>`: a bare
    /// tracked binding stored into a field/array escapes un-followably →
    /// `Dropped`. Anything else is a plain use.
    fn flow_store_value(&mut self, e: &Expr, env: &mut Env) {
        if let Expr::Ident(s) = strip_paren(e) {
            let name = s.text(self.src);
            self.set_dropped(name, env);
            return;
        }
        self.flow_use(e, env);
    }

    /// The workhorse: flow every tracked binding `e` *uses*, applying the right
    /// disposition by syntactic position. Calls / method calls / member reads /
    /// index reads / comparisons **keep `Owned`** (the caller still owns —
    /// Beef passes by reference); address-of / `ref` / `out` / lambda capture
    /// **drop**; an aggregate/store of a bare binding drops.
    ///
    /// A bare `Expr::Ident` reached here is a plain *read* of the binding — it
    /// does NOT move or drop it (reading `p` keeps us the owner). The moving /
    /// dropping positions are handled by the recursive cases below (address-of,
    /// lambda, …) before we ever recurse into the bare ident.
    fn flow_use(&mut self, e: &Expr, env: &mut Env) {
        match e {
            // A bare read of a tracked binding: keep `Owned` (we still own it).
            Expr::Ident(_) => {}
            Expr::Paren { inner, .. } => self.flow_use(inner, env),
            // Address-of / `ref` / `out` of a bare tracked binding: ownership
            // escapes un-followably → Dropped.
            Expr::Unary { op: UnOp::AddrOf, operand, .. } => {
                self.drop_if_bare(operand, env);
            }
            Expr::Prefix { kw: PrefixKw::Ref | PrefixKw::Out, operand, .. } => {
                self.drop_if_bare(operand, env);
            }
            Expr::Unary { operand, .. }
            | Expr::PostInc { operand, .. }
            | Expr::PostDec { operand, .. } => self.flow_use(operand, env),
            Expr::Binary { lhs, rhs, .. } => {
                self.flow_use(lhs, env);
                self.flow_use(rhs, env);
            }
            // A nested plain assignment `(p = q)` as a sub-expression routes
            // through the statement-level handler (so the lattice reset applies);
            // a compound assign (`p += q`) is a read-modify of both operands.
            Expr::Assign { op: AssignOp::Assign, .. } => self.walk_expr(e, env),
            Expr::Assign { target, value, .. } => {
                self.flow_use(target, env);
                self.flow_use(value, env);
            }
            Expr::Ternary { cond, then, els, .. } => {
                self.flow_use(cond, env);
                self.flow_use(then, env);
                self.flow_use(els, env);
            }
            // `f(p)` / `p.M(args)`: an argument pass / method call keeps every
            // bare-ident argument `Owned` (by-reference), and recurses into
            // non-bare args for nested drops.
            Expr::Call { callee, args, .. } => {
                self.flow_use(callee, env);
                for a in args {
                    self.flow_arg(a, env);
                }
            }
            Expr::MixinCall { callee, args, .. } => {
                self.flow_use(callee, env);
                for a in args {
                    self.flow_arg(a, env);
                }
            }
            Expr::Index { base, args, .. } => {
                self.flow_use(base, env);
                for a in args {
                    self.flow_use(a, env);
                }
            }
            Expr::Member { base, .. } => self.flow_use(base, env),
            // Any other keyword-prefixed operand (sizeof/typeof/box/…): plain use.
            Expr::Prefix { operand, .. } => self.flow_use(operand, env),
            Expr::Generic { base, .. } => self.flow_use(base, env),
            Expr::Cast { operand, .. } => self.flow_use(operand, env),
            Expr::DotIdent { .. } => {}
            Expr::Tuple { elems, .. } => {
                // A tuple literal aggregates its elements — a bare tracked
                // binding placed into a tuple escapes un-followably → Dropped.
                for el in elems {
                    self.drop_if_bare(el, env);
                }
            }
            Expr::Initializer { base, entries, .. } => {
                self.flow_use(base, env);
                for en in entries {
                    // An initializer entry `field = p` stores `p` into the new
                    // aggregate → Dropped (un-followable).
                    self.flow_store_value(en, env);
                }
            }
            Expr::Named { value, .. } => self.flow_arg(value, env),
            Expr::Interp { parts, .. } => {
                for p in parts {
                    if let newbf_parser::InterpPart::Hole(h) = p {
                        self.flow_use(h, env);
                    }
                }
            }
            Expr::Lambda { body, .. } => {
                // A lambda may capture a tracked binding — un-followable → drop
                // every binding the body mentions.
                self.drop_lambda(body, env);
            }
            // Literals / this / base / error / sizeof / typeof: no bindings.
            _ => {}
        }
    }

    /// Flow a call/method argument: a bare tracked binding passed positionally
    /// stays in its **owning** state (`Owned`/`OwnedScope`) — Beef passes by
    /// reference, so the callee borrows and does not take ownership; this keeps
    /// leak detection alive across `f(p)` (§B2). A binding that is already
    /// `Deleted`, however, is **untracked** here: matching MS-T5's conservatism,
    /// a use-after-`delete` argument pass stops double-free tracking so a later
    /// `delete p` cannot be (over-)claimed — preserving the exact MS-T5 behavior
    /// (`delete p; f(p); delete p;` stays silent). Non-bare args recurse (so
    /// `f(&p)` / `f(g(p))` still apply the right nested disposition).
    fn flow_arg(&mut self, e: &Expr, env: &mut Env) {
        match strip_paren(e) {
            Expr::Ident(s) => {
                let name = s.text(self.src);
                if let Some(t) = env.get(name)
                    && t.state == State::Deleted
                {
                    env.remove(name);
                }
                // Owning states (Owned/OwnedScope) are kept — borrow, not move.
            }
            other => self.flow_use(other, env),
        }
    }

    /// If `e` is a bare tracked binding, mark it `Dropped` (ownership escaped).
    /// Otherwise flow it as a plain use.
    fn drop_if_bare(&mut self, e: &Expr, env: &mut Env) {
        if let Expr::Ident(s) = strip_paren(e) {
            let name = s.text(self.src);
            self.set_dropped(name, env);
            return;
        }
        self.flow_use(e, env);
    }

    /// Transition a tracked binding to `Moved` (only from an owning state; a
    /// `Deleted`/`Moved`/`Dropped` binding is left as-is).
    fn set_moved(&mut self, name: &str, env: &mut Env) {
        if let Some(t) = env.get_mut(name)
            && matches!(t.state, State::Owned | State::OwnedScope)
        {
            t.state = State::Moved;
        }
    }

    /// Transition a tracked binding to `Dropped` (only from an owning state; a
    /// `Deleted` binding is left as-is so a later double-free is still catchable
    /// — though a dropped binding is normally also un-deletable by the user).
    fn set_dropped(&mut self, name: &str, env: &mut Env) {
        if let Some(t) = env.get_mut(name)
            && matches!(t.state, State::Owned | State::OwnedScope)
        {
            t.state = State::Dropped;
        }
    }

    /// A lambda body can only capture (use) outer bindings — drop every binding
    /// it mentions so a captured owner is no longer tracked (conservative: a
    /// captured binding is `Dropped`, never a leak).
    fn drop_lambda(&mut self, s: &Stmt, env: &mut Env) {
        match s {
            Stmt::Block { stmts, .. } => {
                for st in stmts {
                    self.drop_lambda(st, env);
                }
            }
            Stmt::Expr { expr, .. } => self.drop_all_idents(expr, env),
            Stmt::Return { value: Some(v), .. } => self.drop_all_idents(v, env),
            Stmt::Local { init: Some(i), .. } => self.drop_all_idents(i, env),
            Stmt::If { cond, then, els, .. } => {
                self.drop_all_idents(cond, env);
                self.drop_lambda(then, env);
                if let Some(e) = els {
                    self.drop_lambda(e, env);
                }
            }
            Stmt::While { cond, body, .. } => {
                self.drop_all_idents(cond, env);
                self.drop_lambda(body, env);
            }
            Stmt::For { body, .. } | Stmt::ForEach { body, .. } | Stmt::Defer { body, .. } => {
                self.drop_lambda(body, env);
            }
            _ => {}
        }
    }

    /// Drop every tracked binding mentioned anywhere in an expression — used for
    /// lambda capture, where any reference is an un-followable escape.
    fn drop_all_idents(&mut self, e: &Expr, env: &mut Env) {
        match e {
            Expr::Ident(s) => {
                let name = s.text(self.src);
                self.set_dropped(name, env);
            }
            Expr::Paren { inner, .. } => self.drop_all_idents(inner, env),
            Expr::Unary { operand, .. }
            | Expr::PostInc { operand, .. }
            | Expr::PostDec { operand, .. }
            | Expr::Cast { operand, .. }
            | Expr::Prefix { operand, .. } => self.drop_all_idents(operand, env),
            Expr::Binary { lhs, rhs, .. } => {
                self.drop_all_idents(lhs, env);
                self.drop_all_idents(rhs, env);
            }
            Expr::Assign { target, value, .. } => {
                self.drop_all_idents(target, env);
                self.drop_all_idents(value, env);
            }
            Expr::Ternary { cond, then, els, .. } => {
                self.drop_all_idents(cond, env);
                self.drop_all_idents(then, env);
                self.drop_all_idents(els, env);
            }
            Expr::Call { callee, args, .. } | Expr::MixinCall { callee, args, .. } => {
                self.drop_all_idents(callee, env);
                for a in args {
                    self.drop_all_idents(a, env);
                }
            }
            Expr::Index { base, args, .. } => {
                self.drop_all_idents(base, env);
                for a in args {
                    self.drop_all_idents(a, env);
                }
            }
            Expr::Member { base, .. } => self.drop_all_idents(base, env),
            Expr::Generic { base, .. } => self.drop_all_idents(base, env),
            Expr::Tuple { elems, .. } => {
                for el in elems {
                    self.drop_all_idents(el, env);
                }
            }
            Expr::Initializer { base, entries, .. } => {
                self.drop_all_idents(base, env);
                for en in entries {
                    self.drop_all_idents(en, env);
                }
            }
            Expr::Named { value, .. } => self.drop_all_idents(value, env),
            Expr::Interp { parts, .. } => {
                for p in parts {
                    if let newbf_parser::InterpPart::Hole(h) = p {
                        self.drop_all_idents(h, env);
                    }
                }
            }
            Expr::Lambda { body, .. } => self.drop_lambda(body, env),
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
/// `delete`/leak after a merge is flagged only when the binding is provably in
/// that state on *every* incoming path — the zero-false-positive rule. (An
/// untracked binding is never diagnosed, so "Dropped wins any merge.")
fn merge(a: &Env, b: &Env) -> Env {
    let mut out = HashMap::new();
    for (k, ta) in a {
        if let Some(tb) = b.get(k)
            && ta.state == tb.state
        {
            // Keep `a`'s span (both originate from the same `new` site on the
            // converging paths, so either span is correct).
            out.insert(k.clone(), *ta);
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
