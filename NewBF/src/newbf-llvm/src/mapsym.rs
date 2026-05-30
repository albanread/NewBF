//! Offline crash-dump symbolication against a linker `/MAP` file.
//!
//! The crash dump (`newbf-runtime::crash_dump`) prints raw instruction
//! pointers — the in-box `dbghelp.dll` won't load our own image's PDB to name
//! them. The AOT link emits a `.map` (symbol → `Rva+Base`) next to the exe;
//! this resolves a dump's hex IPs against it, naming **our own** functions
//! with no dbghelp/PDB dependency. It's NewOpenDylan's approach (its
//! `nod-driver symbolicate`): capture raw IPs at the crash, resolve them
//! post-mortem from the `.map`. ASLR is handled by the `runtime_base` slide.

/// One symbol from a `.map`, keyed by its `Rva+Base` (preferred-base address).
struct MapSym {
    rva_plus_base: u64,
    name: String,
}

/// Parse the MSVC `.map` text → `(preferred_base, symbols sorted by address)`.
/// Tolerant: only rows whose first token is `seg:offset` count as symbols.
fn parse_link_map(raw: &str) -> Option<(u64, Vec<MapSym>)> {
    let mut preferred_base: Option<u64> = None;
    let mut syms: Vec<MapSym> = Vec::new();
    let mut past_header = false;
    for line in raw.lines() {
        if preferred_base.is_none()
            && let Some(rest) = line.trim_start().strip_prefix("Preferred load address is ")
        {
            preferred_base = u64::from_str_radix(rest.trim(), 16).ok();
            continue;
        }
        if !past_header {
            if line.trim_start().starts_with("Address ") && line.contains("Rva+Base") {
                past_header = true;
            }
            continue;
        }
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let Some(&first) = tokens.first() else {
            continue;
        };
        if !is_section_offset(first) {
            continue;
        }
        // The `Rva+Base` column is the last 16-hex-digit token on the row.
        let Some(rva_plus_base) = tokens.iter().skip(2).rev().find_map(|t| {
            if t.len() == 16 && t.bytes().all(|b| b.is_ascii_hexdigit()) {
                u64::from_str_radix(t, 16).ok()
            } else {
                None
            }
        }) else {
            continue;
        };
        let Some(&name) = tokens.get(1) else { continue };
        syms.push(MapSym {
            rva_plus_base,
            name: name.to_string(),
        });
    }
    let base = preferred_base?;
    if syms.is_empty() {
        return None;
    }
    syms.sort_by_key(|s| s.rva_plus_base);
    syms.dedup_by(|a, b| a.rva_plus_base == b.rva_plus_base);
    Some((base, syms))
}

/// `seg:offset`, both ≤ 8 hex digits (e.g. `0001:00066ae0`).
fn is_section_offset(s: &str) -> bool {
    let mut parts = s.split(':');
    let (Some(a), Some(b), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let hex = |p: &str| !p.is_empty() && p.len() <= 8 && p.bytes().all(|c| c.is_ascii_hexdigit());
    hex(a) && hex(b)
}

/// The symbol covering `ip` (after removing the ASLR `slide`) + its offset.
fn lookup_symbol(syms: &[MapSym], ip: u64, slide: i64) -> Option<(&str, u64)> {
    let lookup = (ip as i64).checked_sub(slide)? as u64;
    let idx = match syms.binary_search_by_key(&lookup, |s| s.rva_plus_base) {
        Ok(i) => i,
        Err(0) => return None,
        Err(i) => i - 1,
    };
    Some((&syms[idx].name, lookup - syms[idx].rva_plus_base))
}

/// Rewrite every `0x` + 16 hex digits in `text` as `name+0xoff (0x…)` when it
/// resolves to a symbol within 4 MiB (a heuristic to skip unrelated hex).
fn rewrite_hex_ips(text: &str, syms: &[MapSym], slide: i64) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 8);
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 18 <= bytes.len()
            && &bytes[i..i + 2] == b"0x"
            && bytes[i + 2..i + 18].iter().all(|b| b.is_ascii_hexdigit())
            && let Ok(ip) =
                u64::from_str_radix(std::str::from_utf8(&bytes[i + 2..i + 18]).unwrap(), 16)
            && let Some((name, off)) = lookup_symbol(syms, ip, slide)
            && off < 4 * 1024 * 1024
        {
            out.push_str(&format!("{name}+0x{off:x} (0x{ip:016x})"));
            i += 18;
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// Symbolicate a crash dump's raw hex IPs against a linker `.map`.
/// `runtime_base` is the exe's actual load base (for ASLR); `None` assumes the
/// `.map`'s preferred base (no slide). Each resolvable `0x…` IP becomes
/// `name+0xoff (0x…)`; the rest are left untouched.
pub fn symbolicate(
    crash_text: &str,
    map_text: &str,
    runtime_base: Option<u64>,
) -> Result<String, String> {
    let (preferred_base, syms) = parse_link_map(map_text)
        .ok_or_else(|| "could not parse .map (no preferred base / no symbols)".to_string())?;
    let slide = runtime_base.unwrap_or(preferred_base) as i64 - preferred_base as i64;
    Ok(rewrite_hex_ips(crash_text, &syms, slide))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
 my-exe

 Preferred load address is 0000000140000000

  Address         Publics by Value              Rva+Base               Lib:Object

 0001:00001010       main                       0000000140001010 f   foo.obj
 0001:00001240       answer                     0000000140001240 f   foo.obj
";

    #[test]
    fn parses_and_resolves_no_slide() {
        // 0x140001050 is +0x40 into `main`.
        let out = symbolicate("died at 0x0000000140001050 ok", SAMPLE, None).unwrap();
        assert!(out.contains("main+0x40 (0x0000000140001050)"), "{out}");
    }

    #[test]
    fn resolves_with_aslr_slide() {
        // Same offset, but the image is loaded 0x10000 higher at runtime.
        let out = symbolicate("0x0000000140011050", SAMPLE, Some(0x0000_0001_4001_0000)).unwrap();
        assert!(out.contains("main+0x40"), "{out}");
    }

    #[test]
    fn leaves_unknown_ip_untouched() {
        let out = symbolicate("0xdeadbeefdeadbeef", SAMPLE, None).unwrap();
        assert_eq!(out, "0xdeadbeefdeadbeef");
    }

    #[test]
    fn picks_the_nearest_symbol_below() {
        // 0x140001244 is +0x4 into `answer` (the second symbol).
        let out = symbolicate("0x0000000140001244", SAMPLE, None).unwrap();
        assert!(out.contains("answer+0x4"), "{out}");
    }
}
