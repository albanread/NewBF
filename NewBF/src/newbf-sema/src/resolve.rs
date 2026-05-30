//! The resolve pass: resolve `using` targets against the in-program
//! namespace/type tables, and report genuine in-program contradictions
//! (duplicate definitions). Type references that point outside this
//! program (corlib's `System`, primitive names) are recorded as
//! [`UsingRes::External`], *not* flagged — corlib lands in a later sprint,
//! so flagging them now would be noise, not signal.

use std::collections::HashMap;

use crate::Diagnostic;
use crate::build::Builder;
use crate::intern::Symbol;
use crate::model::*;

impl Builder {
    /// Resolve usings and collect duplicate-definition diagnostics.
    pub(crate) fn resolve_and_check(&mut self) -> Vec<Diagnostic> {
        let mut diags = Vec::new();
        self.resolve_usings();
        self.check_duplicate_using_aliases(&mut diags);
        self.check_duplicate_types(&mut diags);
        self.check_duplicate_members(&mut diags);
        diags.sort_by_key(|d| (d.span.file.0, d.span.lo, d.span.hi));
        diags
    }

    fn resolve_usings(&mut self) {
        for i in 0..self.usings.len() {
            let res = match self.dotted_path(&self.usings[i].target) {
                Some(path) => self.resolve_dotted(&path),
                None => UsingRes::External,
            };
            self.usings[i].resolution = res;
        }
    }

    /// Resolve a dotted name to a namespace (preferred) or a type.
    fn resolve_dotted(&self, path: &[String]) -> UsingRes {
        let full = path.join(".");
        if let Some(&ns) = self.ns_index.get(&full) {
            return UsingRes::Namespace(ns);
        }
        // Try `prefix.Type`: the prefix names a namespace, the last segment
        // names a type within it.
        if let Some((last, prefix)) = path.split_last() {
            let prefix_full = prefix.join(".");
            if let Some(&ns) = self.ns_index.get(&prefix_full)
                && let Some(tid) = self.type_in_ns(ns, last)
            {
                return UsingRes::Type(tid);
            }
        }
        UsingRes::External
    }

    fn type_in_ns(&self, ns: NsId, name: &str) -> Option<TypeId> {
        self.namespaces[ns.0 as usize]
            .types
            .iter()
            .copied()
            .find(|&tid| self.interner.resolve(self.types[tid.0 as usize].name) == name)
    }

    /// The dotted segment names of a `TypeRef::Path`, or `None` if the
    /// target isn't a plain path (pointer/array/tuple/etc. — never a using
    /// target in well-formed code).
    fn dotted_path(&self, t: &TypeRef) -> Option<Vec<String>> {
        match t {
            TypeRef::Path { segments, .. } => Some(
                segments
                    .iter()
                    .map(|s| self.interner.resolve(s.name).to_string())
                    .collect(),
            ),
            _ => None,
        }
    }

    // ── duplicate diagnostics ──────────────────────────────────────────────

    fn check_duplicate_using_aliases(&self, diags: &mut Vec<Diagnostic>) {
        // Keyed per-file: the same alias name in two files is fine.
        let mut seen: HashMap<(u32, Symbol), ()> = HashMap::new();
        for u in &self.usings {
            if let Some(alias) = u.alias {
                let key = (u.file.0, alias);
                if seen.insert(key, ()).is_some() {
                    diags.push(Diagnostic {
                        span: u.span,
                        message: format!(
                            "duplicate using alias `{}`",
                            self.interner.resolve(alias)
                        ),
                    });
                }
            }
        }
    }

    /// Two definitional types (not extensions) sharing name + arity in the
    /// same container collide. Extensions reopen an existing type and are
    /// expected to repeat the name, so they're exempt.
    fn check_duplicate_types(&self, diags: &mut Vec<Diagnostic>) {
        // container key: (0, ns_id) for namespace-level, (1, type_id) for nested.
        let mut seen: HashMap<(u8, u32, Symbol, u32), ()> = HashMap::new();
        for t in &self.types {
            if t.kind == TypeKindD::Extension {
                continue;
            }
            let (ck, cid) = match t.enclosing_type {
                Some(tid) => (1u8, tid.0),
                None => (0u8, t.parent_ns.0),
            };
            let key = (ck, cid, t.name, t.arity);
            if seen.insert(key, ()).is_some() {
                let arity = if t.arity == 0 {
                    String::new()
                } else {
                    format!("<{}>", t.arity)
                };
                diags.push(Diagnostic {
                    span: t.span,
                    message: format!(
                        "duplicate type definition `{}{arity}`",
                        self.interner.resolve(t.name)
                    ),
                });
            }
        }
    }

    /// Within a type, field / property / enum-case names must be unique.
    /// Methods (and constructors) overload, so they're excluded.
    fn check_duplicate_members(&self, diags: &mut Vec<Diagnostic>) {
        let mut seen: HashMap<(u32, Symbol), ()> = HashMap::new();
        for m in &self.members {
            let dup_checked = match m {
                MemberDef::Field(_) | MemberDef::EnumCase(_) => true,
                // Indexer properties spell their name `this` and overload by
                // parameter signature, just like methods — exempt them.
                MemberDef::Property(p) => self.interner.resolve(p.name) != "this",
                MemberDef::Method(_) => false,
            };
            if !dup_checked {
                continue;
            }
            let key = (m.owner().0, m.name());
            if seen.insert(key, ()).is_some() {
                diags.push(Diagnostic {
                    span: m.span(),
                    message: format!("duplicate member `{}`", self.interner.resolve(m.name())),
                });
            }
        }
    }
}
