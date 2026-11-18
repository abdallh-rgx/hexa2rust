package demo;

public class MathUtils {
    private static final int MAX = 100;

    private int counter = 0;

    public int add(int a, int b) {
        return a + b;
    }

    public int sumSquares(int n) {
        int total = 0;
        for (int i = 1; i <= n; i++) {
            total += i * i;
        }
        return total;
    }

    public String greet(String name) {
        return "Hello, " + name + "!";
    }

    public static int clamp(int v, int lo, int hi) {
        if (v < lo) return lo;
        if (v > hi) return hi;
        return v;
    }
}
