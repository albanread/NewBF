//! Tokens, spans, keywords, and the source-file interner.

use std::collections::HashMap;

/// Stable id for an interned source file (see [`SourceMap`]).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct FileId(pub u32);

/// A byte range within a source file. `lo`/`hi` are byte offsets; `hi` is
/// exclusive. Spans always land on UTF-8 char boundaries.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Span {
    pub file: FileId,
    pub lo: u32,
    pub hi: u32,
}

impl Span {
    pub fn new(file: FileId, lo: u32, hi: u32) -> Self {
        Self { file, lo, hi }
    }

    pub fn len(self) -> u32 {
        self.hi - self.lo
    }

    pub fn is_empty(self) -> bool {
        self.hi == self.lo
    }

    /// Slice the source text this span covers. The caller must pass the
    /// same source the token was lexed from.
    pub fn text(self, src: &str) -> &str {
        &src[self.lo as usize..self.hi as usize]
    }
}

/// Interns source-file names so spans can reference a small `FileId`
/// instead of a path. Lifted-in-spirit from `newm2-lexer`'s interner.
#[derive(Default)]
pub struct SourceMap {
    names: Vec<String>,
    index: HashMap<String, FileId>,
}

impl SourceMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn intern(&mut self, name: &str) -> FileId {
        if let Some(&id) = self.index.get(name) {
            return id;
        }
        let id = FileId(self.names.len() as u32);
        self.names.push(name.to_owned());
        self.index.insert(name.to_owned(), id);
        id
    }

    pub fn name(&self, id: FileId) -> Option<&str> {
        self.names.get(id.0 as usize).map(String::as_str)
    }
}

macro_rules! keywords {
    ($($variant:ident => $text:literal),+ $(,)?) => {
        /// Beef reserved words. The set is lifted verbatim from upstream
        /// Beef's tokenizer (`E:\beef\IDEHelper\Compiler\BfParser.cpp`,
        /// the `SrcPtrHasToken` table). Primitive type names (`int`,
        /// `float`, `bool`, `void`, `char8`, …) are deliberately *not*
        /// here — Beef lexes them as identifiers and resolves them as
        /// types later, so we do the same.
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        pub enum Keyword { $($variant),+ }

        impl Keyword {
            /// Returns the keyword for an identifier string, if it is one.
            pub fn from_ident(s: &str) -> Option<Keyword> {
                match s {
                    $($text => Some(Keyword::$variant),)+
                    _ => None,
                }
            }

            /// The canonical spelling of this keyword.
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Keyword::$variant => $text,)+
                }
            }
        }
    };
}

keywords! {
    Abstract => "abstract", AllocType => "alloctype", AlignOf => "alignof",
    Append => "append", As => "as", Asm => "asm", Base => "base", Box => "box",
    Break => "break", Case => "case", Catch => "catch", Checked => "checked",
    Class => "class", Comptype => "comptype", Concrete => "concrete",
    Const => "const", Continue => "continue", Decltype => "decltype",
    Default => "default", Defer => "defer", Delegate => "delegate",
    Delete => "delete", Do => "do", Else => "else", Enum => "enum",
    Explicit => "explicit", Extern => "extern", Extension => "extension",
    Fallthrough => "fallthrough", False => "false", Finally => "finally",
    Fixed => "fixed", For => "for", Function => "function", Goto => "goto",
    If => "if", Implicit => "implicit", In => "in", Inline => "inline",
    Interface => "interface", Internal => "internal", Is => "is",
    IsConst => "isconst", Let => "let", Mixin => "mixin", Mut => "mut",
    Namespace => "namespace", NameOf => "nameof", New => "new", Null => "null",
    Nullable => "nullable", OffsetOf => "offsetof", Operator => "operator",
    Out => "out", Override => "override", Params => "params",
    Private => "private", Protected => "protected", Public => "public",
    ReadOnly => "readonly", Ref => "ref", Repeat => "repeat",
    RetType => "rettype", Return => "return", Scope => "scope",
    Sealed => "sealed", SizeOf => "sizeof", Static => "static",
    StrideOf => "strideof", Struct => "struct", Switch => "switch",
    This => "this", Throw => "throw", True => "true", Try => "try",
    TypeOf => "typeof", TypeAlias => "typealias", Unchecked => "unchecked",
    Using => "using", Var => "var", Virtual => "virtual", Volatile => "volatile",
    When => "when", Where => "where", While => "while", Yield => "yield",
}

/// What a [`Token`] is. The lexer is *lossless*: trivia (whitespace and
/// comments) are emitted as tokens too, so the token spans tile the
/// entire source with no gaps — concatenating each token's text
/// reconstructs the input exactly.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TokenKind {
    // ── trivia ────────────────────────────────────────────────────────
    Whitespace,
    LineComment,  // `// …`
    DocComment,   // `/// …`
    BlockComment, // `/* … */` (nesting supported)
    PreprocLine,  // `#unwarn` / `#pragma …` / `#if …` (consumed to EOL)

    // ── literals ──────────────────────────────────────────────────────
    Int,         // 123, 0xFF, 0b1010, with `_` separators and suffixes
    Float,       // 1.0, .5, 1e9, 1.0f
    Char,        // 'a', '\n'
    Str,         // "…"
    VerbatimStr, // @"…"
    InterpStr,   // $"…" (and $@"…" / @$"…")

    // ── words ─────────────────────────────────────────────────────────
    Ident,
    Keyword(Keyword),

    // ── grouping / punctuation ────────────────────────────────────────
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Semicolon,
    Comma,
    Dot,
    DotDot,
    DotDotDot,  // ... (closed/inclusive range)
    DotDotLess, // ..< (half-open/exclusive range)
    Colon,
    ColonColon,
    Question,
    QuestionQuestion,
    QuestionDot,
    QuestionQuestionEq,
    Arrow,    // ->
    FatArrow, // =>
    Pound,    // # (preprocessor directives)

    // ── operators ─────────────────────────────────────────────────────
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Amp,
    Pipe,
    Caret,
    Tilde,
    Bang,
    Assign,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    PercentEq,
    AmpEq,
    PipeEq,
    CaretEq,
    ShlEq,
    ShrEq,
    EqEq,
    NotEq,
    StrictEq,  // === (reference identity)
    StrictNeq, // !==
    Lt,
    Gt,
    Le,
    Ge,
    Spaceship, // <=> (three-way compare)
    AmpAmp,
    PipePipe,
    Shl,
    Shr,
    PlusPlus,
    MinusMinus,
    // Beef overflow arithmetic operators.
    AmpPlus,    // &+
    AmpMinus,   // &-
    AmpStar,    // &*
    AmpPlusEq,  // &+=
    AmpMinusEq, // &-=
    AmpStarEq,  // &*=
    At,
    Dollar,

    // ── catch-alls ────────────────────────────────────────────────────
    Unknown, // a byte/char we don't recognise (kept so spans stay total)
    Eof,
}

impl TokenKind {
    /// True for whitespace and comments — the tokens a parser ignores.
    pub fn is_trivia(self) -> bool {
        matches!(
            self,
            TokenKind::Whitespace
                | TokenKind::LineComment
                | TokenKind::DocComment
                | TokenKind::BlockComment
                | TokenKind::PreprocLine
        )
    }
}

/// A lexed token: its kind plus the source span it covers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}
