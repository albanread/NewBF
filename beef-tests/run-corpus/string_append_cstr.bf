// expect: 1
// corlib String.Append(char8*): appends the bytes of a NUL-terminated C string
// (a string literal lowers to char8*), selected over the char8/String/int/bool
// overloads by the argument's type. This is the overload CR-T4 uses to append a
// reflected field NAME (FieldInfo.GetName() → char8*) into an emitted String.
//
// Start empty, Append("mX") (the char8* overload), then Append('!') (char8) to
// confirm the char8* path leaves a well-formed buffer the existing overloads
// still extend. Result must be "mX!": length 3, chars 'm','X','!'.
class Program {
	public static int32 Main() {
		String s = new String();
		s.Append("mX");               // Append(char8*): copies 'm','X' (stops at NUL)
		s.Append('!');                // Append(char8): still resolves as before
		bool ok = s.Length() == 3
			&& s.CharAt(0) == 'm'
			&& s.CharAt(1) == 'X'
			&& s.CharAt(2) == '!';
		delete s;
		return ok ? 1 : 0;
	}
}
