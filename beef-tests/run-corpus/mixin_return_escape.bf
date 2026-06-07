// expect: 7
// MX-T4 — control-flow ESCAPE targets the CALLER (mixins.md §3.6). The mixin
// `Bail!()` body is `return 7;` — a `return` STATEMENT spliced into Main. Because
// the splice reuses the live Lowerer, that `return` lowers through the ordinary
// `Stmt::Return` arm against the CALLER's (Main's) `ret_ty`, so it exits MAIN, not
// any synthetic callee. The `return 99;` after the mixin call is therefore DEAD
// (the splice set `terminated`), and Main returns 7.
//
// This pins: (a) escape exits the caller, (b) the statement after an escaping
// mixin call is correctly skipped (terminated), (c) no panic / no stack desync.
class Program {
	static mixin Bail() {
		return 7;
	}
	public static int32 Main() {
		Bail!();      // splices `return 7;` → exits Main with 7
		return 99;    // dead: the splice terminated the block
	}
}
