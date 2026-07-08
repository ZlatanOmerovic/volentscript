package main

import (
	"fmt"
	"math"
)

const N = 2500

func evalA(i, j int) float64 {
	ij := i + j
	return 1.0 / float64((ij*(ij+1))/2+i+1)
}

func multiplyAv(v, av []float64) {
	for i := 0; i < N; i++ {
		sum := 0.0
		for j := 0; j < N; j++ {
			sum += evalA(i, j) * v[j]
		}
		av[i] = sum
	}
}

func multiplyAtv(v, atv []float64) {
	for i := 0; i < N; i++ {
		sum := 0.0
		for j := 0; j < N; j++ {
			sum += evalA(j, i) * v[j]
		}
		atv[i] = sum
	}
}

func multiplyAtAv(v, atav, tmp []float64) {
	multiplyAv(v, tmp)
	multiplyAtv(tmp, atav)
}

func main() {
	u := make([]float64, N)
	v := make([]float64, N)
	tmp := make([]float64, N)
	for k := 0; k < N; k++ {
		u[k] = 1.0
	}
	for iter := 0; iter < 10; iter++ {
		multiplyAtAv(u, v, tmp)
		multiplyAtAv(v, u, tmp)
	}
	vBv, vv := 0.0, 0.0
	for i := 0; i < N; i++ {
		vBv += u[i] * v[i]
		vv += v[i] * v[i]
	}
	fmt.Printf("%.9f\n", math.Sqrt(vBv/vv))
}
