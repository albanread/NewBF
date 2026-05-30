// expect: 1
class Program {
    public static int32 IsEven(int32 n) { if (n == 0) { return 1; } return IsOdd(n - 1); }
    public static int32 IsOdd(int32 n) { if (n == 0) { return 0; } return IsEven(n - 1); }
    public static int32 Main() => IsEven(10);
}
