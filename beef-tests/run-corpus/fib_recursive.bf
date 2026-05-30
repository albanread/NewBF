// expect: 55
class Program {
    public static int32 Fib(int32 n) {
        if (n < 2) { return n; }
        return Fib(n - 1) + Fib(n - 2);
    }
    public static int32 Main() => Fib(10);
}
