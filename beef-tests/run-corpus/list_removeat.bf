// expect: 470
// List<T>.RemoveAt(index) deletes one element, shifts the tail down a slot, and
// decrements the count; Clear() zeroes the count (buffer kept).
//   xs = [10, 20, 30, 40]            (Count 4)
//   xs.RemoveAt(1)  removes 20  ->  [10, 30, 40]   (Count 3; 30,40 shifted down)
// Pack the post-remove state into one int (each slot proves a shift held):
//   Count() * 100                  = 3 * 100        = 300
//   Get(0) * 10                    = 10 * 10        = 100
//   Get(1)                         = 30   (the old index-2 element shifted in)
//   Get(2)                         = 40
//   subtotal                                         = 470
//   then Clear(): Count() must be 0; add Count() * 1000 (= 0 iff Clear worked).
//   total                          = 470 + 0 * 1000  = 470
class Program {
	public static int32 Main() {
		List<int32> xs = new List<int32>();
		xs.Add(10);
		xs.Add(20);
		xs.Add(30);
		xs.Add(40);

		xs.RemoveAt(1);
		int32 r = xs.Count() * 100 + xs.Get(0) * 10 + xs.Get(1) + xs.Get(2);

		xs.Clear();
		r = r + xs.Count() * 1000;

		delete xs;
		return r;
	}
}
