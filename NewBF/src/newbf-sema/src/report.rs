//! `dump-defs` — the schema-stable definition-graph report. One block per
//! type, listing its full shape and every member, so the exhaustiveness of
//! the build pass is auditable by eye.

use std::fmt::Write;

use crate::Program;
use crate::intern::Interner;
use crate::model::*;

/// Render the whole definition graph as a deterministic report.
pub fn format_defs(p: &Program) -> String {
    let g = &p.graph;
    let it = &p.interner;
    let mut out = String::new();

    let _ = writeln!(
        out,
        "defs: {} namespaces, {} types, {} members, {} usings, {} symbols",
        g.namespaces.len(),
        g.types.len(),
        g.members.len(),
        g.usings.len(),
        it.len(),
    );

    // ── usings ──────────────────────────────────────────────────────────
    if !g.usings.is_empty() {
        let _ = writeln!(out, "\nusings:");
        for u in &g.usings {
            let kw = if u.is_static { "using static" } else { "using" };
            let alias = match u.alias {
                Some(a) => format!("{} = ", it.resolve(a)),
                None => String::new(),
            };
            let res = match u.resolution {
                UsingRes::Namespace(_) => "namespace",
                UsingRes::Type(_) => "type",
                UsingRes::External => "external",
            };
            let _ = writeln!(
                out,
                "  {kw} {alias}{}  -> {res}",
                fmt_typeref(it, &u.target)
            );
        }
    }

    // ── namespaces ──────────────────────────────────────────────────────
    let _ = writeln!(out, "\nnamespaces:");
    for ns in &g.namespaces {
        let name = if ns.full.is_empty() {
            "(global)"
        } else {
            ns.full.as_str()
        };
        let _ = writeln!(out, "  {name}  ({} types)", ns.types.len());
    }

    // ── types ───────────────────────────────────────────────────────────
    let _ = writeln!(out, "\ntypes:");
    for (i, t) in g.types.iter().enumerate() {
        let tid = TypeId(i as u32);
        let mods = fmt_mods(&t.modifiers);
        let _ = writeln!(
            out,
            "  {} : {}{}{}",
            type_qualname(g, it, tid),
            mods,
            t.kind.as_str(),
            fmt_generics(it, &t.generic_params),
        );
        for a in &t.attributes {
            let _ = writeln!(
                out,
                "      [attr {} ({} args)]",
                fmt_typeref(it, &a.name),
                a.arg_count
            );
        }
        if !t.bases.is_empty() {
            let bases: Vec<String> = t.bases.iter().map(|b| fmt_typeref(it, b)).collect();
            let _ = writeln!(out, "      bases: {}", bases.join(", "));
        }
        for w in &t.constraints {
            let cs: Vec<String> = w.constraints.iter().map(|c| fmt_typeref(it, c)).collect();
            let _ = writeln!(
                out,
                "      where {} : {}",
                it.resolve(w.name),
                cs.join(", ")
            );
        }
        if let Some(target) = &t.alias_target {
            let _ = writeln!(out, "      = {}", fmt_typeref(it, target));
        }
        if let Some(sig) = &t.delegate_sig {
            let _ = writeln!(
                out,
                "      sig: ({}) -> {}",
                fmt_params(it, &sig.params),
                fmt_typeref(it, &sig.return_ty),
            );
        }
        for &mid in &t.members {
            let _ = writeln!(out, "      {}", fmt_member(it, g.member(mid)));
        }
        if !t.nested_types.is_empty() {
            let names: Vec<String> = t
                .nested_types
                .iter()
                .map(|&n| it.resolve(g.ty(n).name).to_string())
                .collect();
            let _ = writeln!(out, "      nested: {}", names.join(", "));
        }
    }

    out
}

fn fmt_member(it: &Interner, m: &MemberDef) -> String {
    match m {
        MemberDef::Field(f) => {
            let init = if f.has_init { " = …" } else { "" };
            format!(
                "field {} : {}{}{}",
                it.resolve(f.name),
                fmt_mods(&f.modifiers),
                fmt_typeref(it, &f.ty),
                init,
            )
        }
        MemberDef::Method(m) => {
            let ret = match &m.return_ty {
                Some(r) => format!(" -> {}", fmt_typeref(it, r)),
                None => String::new(),
            };
            format!(
                "{} {}{}{}({}){} body={}",
                m.method_kind.as_str(),
                fmt_mods(&m.modifiers),
                fmt_iface(it, &m.explicit_iface),
                it.resolve(m.name),
                fmt_params(it, &m.params),
                ret,
                m.body.as_str(),
            )
        }
        MemberDef::Property(p) => {
            let accs: Vec<&str> = p.accessors.iter().map(|a| a.kind.as_str()).collect();
            format!(
                "property {}{} : {}{} {{ {} }}",
                fmt_iface(it, &p.explicit_iface),
                it.resolve(p.name),
                fmt_mods(&p.modifiers),
                fmt_typeref(it, &p.ty),
                accs.join(" "),
            )
        }
        MemberDef::EnumCase(c) => {
            let payload = if c.payload.is_empty() {
                String::new()
            } else {
                format!("({})", fmt_params(it, &c.payload))
            };
            let val = if c.has_value { " = …" } else { "" };
            format!("case {}{}{}", it.resolve(c.name), payload, val)
        }
    }
}

/// Render the explicit-interface qualifier prefix (`IFace.`) for a member
/// that explicitly implements an interface, or empty otherwise.
fn fmt_iface(it: &Interner, iface: &Option<TypeRef>) -> String {
    match iface {
        Some(t) => format!("{}.", fmt_typeref(it, t)),
        None => String::new(),
    }
}

fn fmt_params(it: &Interner, params: &[ParamDef]) -> String {
    params
        .iter()
        .map(|p| {
            let m = match p.modifier {
                Some(m) => format!("{} ", m.as_str()),
                None => String::new(),
            };
            let name = match p.name {
                Some(n) => format!(" {}", it.resolve(n)),
                None => String::new(),
            };
            let def = if p.has_default { " = …" } else { "" };
            format!("{m}{}{name}{def}", fmt_typeref(it, &p.ty))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn fmt_mods(mods: &[newbf_parser::Modifier]) -> String {
    if mods.is_empty() {
        return String::new();
    }
    let mut s: String = mods
        .iter()
        .map(|m| m.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    s.push(' ');
    s
}

fn fmt_generics(it: &Interner, gps: &[crate::intern::Symbol]) -> String {
    if gps.is_empty() {
        return String::new();
    }
    let names: Vec<&str> = gps.iter().map(|&g| it.resolve(g)).collect();
    format!("<{}>", names.join(", "))
}

fn type_qualname(g: &DefGraph, it: &Interner, tid: TypeId) -> String {
    let t = g.ty(tid);
    let name = it.resolve(t.name);
    match t.enclosing_type {
        Some(e) => format!("{}.{}", type_qualname(g, it, e), name),
        None => {
            let ns = g.ns(t.parent_ns);
            if ns.full.is_empty() {
                name.to_string()
            } else {
                format!("{}.{}", ns.full, name)
            }
        }
    }
}

/// Render a normalized type reference (mirrors the parser's `sxt` shape but
/// reads through the interner).
fn fmt_typeref(it: &Interner, t: &TypeRef) -> String {
    match t {
        TypeRef::Var(_) => "var".to_string(),
        TypeRef::Error(_) => "<error>".to_string(),
        TypeRef::Path { segments, .. } => {
            let mut s = String::new();
            for (i, seg) in segments.iter().enumerate() {
                if i > 0 {
                    s.push('.');
                }
                s.push_str(it.resolve(seg.name));
                if !seg.args.is_empty() {
                    s.push('<');
                    let args: Vec<String> = seg.args.iter().map(|a| fmt_typeref(it, a)).collect();
                    s.push_str(&args.join(", "));
                    s.push('>');
                }
            }
            s
        }
        TypeRef::Pointer { inner, .. } => format!("{}*", fmt_typeref(it, inner)),
        TypeRef::Nullable { inner, .. } => format!("{}?", fmt_typeref(it, inner)),
        TypeRef::Array { inner, rank, .. } => {
            let commas = ",".repeat((*rank as usize).saturating_sub(1));
            format!("{}[{commas}]", fmt_typeref(it, inner))
        }
        TypeRef::Sized { inner, .. } => format!("{}[N]", fmt_typeref(it, inner)),
        TypeRef::Tuple { elems, .. } => {
            let es: Vec<String> = elems.iter().map(|e| fmt_typeref(it, e)).collect();
            format!("({})", es.join(", "))
        }
        TypeRef::Function {
            is_delegate,
            return_ty,
            params,
            ..
        } => {
            let kw = if *is_delegate { "delegate" } else { "function" };
            let ps: Vec<String> = params.iter().map(|p| fmt_typeref(it, p)).collect();
            format!("{kw} {}({})", fmt_typeref(it, return_ty), ps.join(", "))
        }
    }
}
