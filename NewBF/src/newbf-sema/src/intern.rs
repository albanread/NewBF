//! A small string interner. Every identifier captured by the def builder
//! is interned to a [`Symbol`] so the model stores `u32`s, not owned
//! strings, and name comparisons are integer-equality.

use std::collections::HashMap;

/// An interned string. Cheap to copy and compare.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Symbol(pub u32);

/// Interns strings to [`Symbol`]s. The empty string is always `Symbol(0)`
/// so the global (unnamed) namespace has a stable id.
pub struct Interner {
    strings: Vec<String>,
    index: HashMap<String, Symbol>,
}

impl Default for Interner {
    fn default() -> Self {
        let mut me = Self {
            strings: Vec::new(),
            index: HashMap::new(),
        };
        // Reserve Symbol(0) for the empty string.
        me.intern("");
        me
    }
}

impl Interner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn intern(&mut self, s: &str) -> Symbol {
        if let Some(&sym) = self.index.get(s) {
            return sym;
        }
        let sym = Symbol(self.strings.len() as u32);
        self.strings.push(s.to_owned());
        self.index.insert(s.to_owned(), sym);
        sym
    }

    /// The empty-string symbol (the global namespace's name).
    pub fn empty(&self) -> Symbol {
        Symbol(0)
    }

    pub fn resolve(&self, sym: Symbol) -> &str {
        &self.strings[sym.0 as usize]
    }

    pub fn len(&self) -> usize {
        self.strings.len()
    }

    pub fn is_empty(&self) -> bool {
        // Always holds the reserved empty string, so never truly empty.
        self.strings.is_empty()
    }
}
