// expect: 120
class Program {
    public static int32 Main() {
        int32 n = 5;
        int32 acc = 1;
        while (n > 1) { acc = acc * n; n = n - 1; }
        return acc;
    }
}
