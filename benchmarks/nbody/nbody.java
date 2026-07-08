public class nbody {
    static final double PI = 3.141592653589793, SM = 4 * PI * PI, DPY = 365.24;
    static double[][] b = {
        {0, 0, 0, 0, 0, 0, SM},
        {4.84143144246472090, -1.16032004402742839, -0.103622044471123109,
         0.00166007664274403694 * DPY, 0.00769901118419740425 * DPY, -0.0000690460016972063023 * DPY,
         0.000954791938424326609 * SM},
        {8.34336671824457987, 4.12479856412430479, -0.403523417114321381,
         -0.00276742510726862411 * DPY, 0.00499852801234917238 * DPY, 0.0000230417297573763929 * DPY,
         0.000285885980666130812 * SM},
        {12.8943695621391310, -15.1111514016986312, -0.223307578892655734,
         0.00296460137564761618 * DPY, 0.00237847173959480950 * DPY, -0.0000296589568540237556 * DPY,
         0.0000436624404335156298 * SM},
        {15.3796971148509165, -25.9193146099879641, 0.179258772950371181,
         0.00268067772490389322 * DPY, 0.00162824170038242295 * DPY, -0.0000951592254519715870 * DPY,
         0.0000515138902046611451 * SM}};
    static double energy() {
        double e = 0;
        for (int i = 0; i < 5; i++) {
            e += 0.5 * b[i][6] * (b[i][3] * b[i][3] + b[i][4] * b[i][4] + b[i][5] * b[i][5]);
            for (int j = i + 1; j < 5; j++) {
                double dx = b[i][0] - b[j][0], dy = b[i][1] - b[j][1], dz = b[i][2] - b[j][2];
                e -= (b[i][6] * b[j][6]) / Math.sqrt(dx * dx + dy * dy + dz * dz);
            }
        }
        return e;
    }
    static void advance(double dt) {
        for (int i = 0; i < 5; i++) {
            for (int j = i + 1; j < 5; j++) {
                double dx = b[i][0] - b[j][0], dy = b[i][1] - b[j][1], dz = b[i][2] - b[j][2];
                double d2 = dx * dx + dy * dy + dz * dz;
                double mag = dt / (d2 * Math.sqrt(d2));
                b[i][3] -= dx * b[j][6] * mag; b[i][4] -= dy * b[j][6] * mag; b[i][5] -= dz * b[j][6] * mag;
                b[j][3] += dx * b[i][6] * mag; b[j][4] += dy * b[i][6] * mag; b[j][5] += dz * b[i][6] * mag;
            }
            b[i][0] += dt * b[i][3]; b[i][1] += dt * b[i][4]; b[i][2] += dt * b[i][5];
        }
    }
    public static void main(String[] a) {
        double px = 0, py = 0, pz = 0;
        for (int i = 0; i < 5; i++) { px += b[i][3] * b[i][6]; py += b[i][4] * b[i][6]; pz += b[i][5] * b[i][6]; }
        b[0][3] = -px / SM; b[0][4] = -py / SM; b[0][5] = -pz / SM;
        System.out.printf("%.9f%n", energy());
        for (int s = 0; s < 5000000; s++) advance(0.01);
        System.out.printf("%.9f%n", energy());
    }
}
