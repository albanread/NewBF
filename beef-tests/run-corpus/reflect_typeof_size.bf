// expect: 16
// RF-T4: typeof(T).GetSize() returns the object instance size from the backend
// DataLayout. TwoInts is a class: $header (ptr, 8) + mX (i32, 4) + mY (i32, 4)
// = 16 bytes. Pins mSize to the real get_size value.
[Reflect] class TwoInts { public int32 mX; public int32 mY; }
class Program {
	public static int32 Main() {
		return typeof(TwoInts).GetSize();
	}
}
