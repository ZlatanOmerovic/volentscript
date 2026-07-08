#include <stdio.h>
#include <math.h>

#define N 2500

static double evalA(int i, int j) {
    int ij = i + j;
    return 1.0 / ((ij * (ij + 1)) / 2 + i + 1);
}

static void multiplyAv(const double *v, double *av) {
    for (int i = 0; i < N; i++) {
        double sum = 0.0;
        for (int j = 0; j < N; j++) sum += evalA(i, j) * v[j];
        av[i] = sum;
    }
}

static void multiplyAtv(const double *v, double *atv) {
    for (int i = 0; i < N; i++) {
        double sum = 0.0;
        for (int j = 0; j < N; j++) sum += evalA(j, i) * v[j];
        atv[i] = sum;
    }
}

static void multiplyAtAv(const double *v, double *atav, double *tmp) {
    multiplyAv(v, tmp);
    multiplyAtv(tmp, atav);
}

int main(void) {
    static double u[N], v[N], tmp[N];
    for (int k = 0; k < N; k++) { u[k] = 1.0; v[k] = 0.0; tmp[k] = 0.0; }
    for (int iter = 0; iter < 10; iter++) {
        multiplyAtAv(u, v, tmp);
        multiplyAtAv(v, u, tmp);
    }
    double vBv = 0.0, vv = 0.0;
    for (int i = 0; i < N; i++) { vBv += u[i] * v[i]; vv += v[i] * v[i]; }
    printf("%.9f\n", sqrt(vBv / vv));
    return 0;
}
