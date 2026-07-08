public class spectralnorm {
    static final int N = 2500;

    static double evalA(int i, int j) {
        int ij = i + j;
        return 1.0 / ((ij * (ij + 1)) / 2 + i + 1);
    }

    static void multiplyAv(double[] v, double[] av) {
        for (int i = 0; i < N; i++) {
            double sum = 0.0;
            for (int j = 0; j < N; j++) sum += evalA(i, j) * v[j];
            av[i] = sum;
        }
    }

    static void multiplyAtv(double[] v, double[] atv) {
        for (int i = 0; i < N; i++) {
            double sum = 0.0;
            for (int j = 0; j < N; j++) sum += evalA(j, i) * v[j];
            atv[i] = sum;
        }
    }

    static void multiplyAtAv(double[] v, double[] atav, double[] tmp) {
        multiplyAv(v, tmp);
        multiplyAtv(tmp, atav);
    }

    public static void main(String[] args) {
        double[] u = new double[N];
        double[] v = new double[N];
        double[] tmp = new double[N];
        for (int k = 0; k < N; k++) u[k] = 1.0;
        for (int iter = 0; iter < 10; iter++) {
            multiplyAtAv(u, v, tmp);
            multiplyAtAv(v, u, tmp);
        }
        double vBv = 0.0, vv = 0.0;
        for (int i = 0; i < N; i++) { vBv += u[i] * v[i]; vv += v[i] * v[i]; }
        System.out.printf("%.9f%n", Math.sqrt(vBv / vv));
    }
}
