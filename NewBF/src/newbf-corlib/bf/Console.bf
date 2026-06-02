// NewBF corlib — Console: text output over the Win32 console API.
//
// Uses GetStdHandle + WriteFile directly. WriteFile is unbuffered and
// length-based, so output is captured correctly even when stdout is redirected
// to a pipe or file (unlike WriteConsole, which needs a real console). The
// `[LinkName]` externs resolve from kernel32 via the JIT's process search
// generator — the same path the Internal allocator floor uses for the CRT.
class Console {
	[LinkName("GetStdHandle")]
	public static extern void* GetStdHandle(int32 nStdHandle);
	[LinkName("WriteFile")]
	public static extern int32 WriteFile(void* handle, void* buffer, int32 bytes, void* written, void* overlapped);

	// STD_OUTPUT_HANDLE is -11. The bytes-written out-param must be non-null for
	// a synchronous write, so borrow a scratch cell from the allocator floor.
	static void WriteBytes(void* buffer, int32 bytes) {
		void* handle = Console.GetStdHandle(-11);
		void* written = Internal.Malloc(8);
		Console.WriteFile(handle, buffer, bytes, written, null);
		Internal.Free(written);
	}

	public static void Write(String s) {
		Console.WriteBytes(s.Ptr(), s.Length());
	}

	public static void WriteLine(String s) {
		Console.Write(s);
		String nl = "\n";
		Console.Write(nl);
		delete nl;
	}

	// Decimal rendering of an int, printed with a trailing newline. Selected
	// over WriteLine(String) by the argument's type (overload resolution).
	public static void WriteLine(int n) {
		String s = new String();
		if (n < 0) {
			s.Append('-');
			Console.AppendDigits(s, -n);
		} else {
			Console.AppendDigits(s, n);
		}
		Console.WriteLine(s);
		delete s;
	}

	// Print a bool as `true`/`false` with a trailing newline. Selected over the
	// int/String overloads by the argument's type.
	public static void WriteLine(bool b) {
		if (b) {
			String t = "true";
			Console.WriteLine(t);
			delete t;
		} else {
			String f = "false";
			Console.WriteLine(f);
			delete f;
		}
	}

	// Append n's decimal digits most-significant-first: the recursion emits the
	// high digits before appending the current low one.
	static void AppendDigits(String s, int n) {
		if (n >= 10) {
			Console.AppendDigits(s, n / 10);
		}
		int d = n % 10;
		char8 c = '0' + d;
		s.Append(c);
	}
}
