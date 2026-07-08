public class mandelbrot {
    public static void main(String[] a) {
        int size = 1500, inside = 0;
        for (int py = 0; py < size; py++) {
            double ci = 2.0 * py / size - 1.0;
            for (int px = 0; px < size; px++) {
                double cr = 2.0 * px / size - 1.5;
                double zr = 0, zi = 0;
                int k = 0;
                while (k < 50 && zr * zr + zi * zi <= 4.0) {
                    double t = zr * zr - zi * zi + cr;
                    zi = 2.0 * zr * zi + ci;
                    zr = t;
                    k++;
                }
                if (k == 50) inside++;
            }
        }
        System.out.println(inside);
    }
}
