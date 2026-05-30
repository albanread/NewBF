// expect: 233
class Program {
    public static int32 Main() {
        int32 a = 0;
        int32 b = 1;
        int32 t = 0;
        int32 n = 13;
        while (n > 0) { t = a + b; a = b; b = t; n = n - 1; }
        return a;
    }
}
