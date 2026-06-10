// IT-T4 verify-corpus pin: the FIFTH `foreach` branch — the GetEnumerator
// enumerator protocol over a USER type — lowered to a *verifiable* LLVM module.
//
// This mirrors the run-corpus `foreach_getenumerator.bf` shape (a user value
// struct exposing `GetEnumerator()` returning a value-struct enumerator cursor
// with `MoveNext() -> bool` + a `Current` property, iterated by `foreach`), but
// is self-contained — an inline `int32[3]` buffer instead of `Internal.Malloc`
// — so it builds and verifies clean as a standalone one-file program (the verify
// corpus analyzes each file without the corlib prelude).
//
// What it pins: the GetEnumerator loop lowers to the head/body/cont/exit block
// skeleton with the value-struct enumerator `this`-pointer reused across
// iterations (MoveNext mutates `mIndex`; Current reads through the same slot) +
// the optional `Dispose()` scope-cleanup hook — and the whole shape is internally
// type-consistent SSA (every block terminated, every use width-correct), which is
// exactly what `newbf_llvm::verify_module` checks. Raises the verify/parser
// denominator by 1.
struct BagEnumerator
{
	int32* mItems;
	int32 mCount;
	int32 mIndex;       // -1 before the first MoveNext

	public bool MoveNext()
	{
		this.mIndex = this.mIndex + 1;
		return this.mIndex < this.mCount;
	}
	public int32 Current { get { return this.mItems[this.mIndex]; } }
	public void Dispose() { }
}

struct Bag
{
	int32* mItems;
	int32 mCount;

	public BagEnumerator GetEnumerator()
	{
		BagEnumerator e = ?;
		e.mItems = this.mItems;
		e.mCount = this.mCount;
		e.mIndex = -1;
		return e;
	}
}

class Program
{
	public static int32 Main()
	{
		int32[3] buf = ?;
		buf[0] = 10;
		buf[1] = 20;
		buf[2] = 30;
		Bag bag = ?;
		bag.mItems = &buf[0];
		bag.mCount = 3;
		int32 sum = 0;
		for (var x in bag)
		{
			sum += x;
		}
		return sum;   // 10 + 20 + 30 = 60
	}
}
