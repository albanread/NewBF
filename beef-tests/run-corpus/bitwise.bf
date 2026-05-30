// expect: 255
class Program { public static int32 Main() => (0xF0 | 0x0F) & 0xFF; }
