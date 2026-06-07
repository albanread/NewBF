// expect: 1
// RF-T4 standalone StrEq smoke: a char8*-vs-char8* NUL-terminated compare
// (Internal.StrEq). Disambiguates a StrEq bug from a metadata bug — if this
// fails, reflect_typeof_name's failure is StrEq's, not the Type global's.
// String literals lower to char8*, so this is the natural comparison.
class Program {
	public static int32 Main() {
		bool eq = Internal.StrEq("ab", "ab");
		bool ne = Internal.StrEq("ab", "ac");
		return (eq && !ne) ? 1 : 0;
	}
}
