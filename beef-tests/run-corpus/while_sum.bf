// expect: 55
class Program {
    public static int32 Main() {
        int32 sum = 0;
        int32 i = 1;
        while (i <= 10) { sum = sum + i; i = i + 1; }
        return sum;
    }
}
