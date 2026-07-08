package main

import "fmt"

func main() {
	size := 1500
	inside := 0
	for py := 0; py < size; py++ {
		ci := 2.0*float64(py)/float64(size) - 1.0
		for px := 0; px < size; px++ {
			cr := 2.0*float64(px)/float64(size) - 1.5
			zr, zi := 0.0, 0.0
			k := 0
			for k < 50 && zr*zr+zi*zi <= 4.0 {
				t := zr*zr - zi*zi + cr
				zi = 2.0*zr*zi + ci
				zr = t
				k++
			}
			if k == 50 {
				inside++
			}
		}
	}
	fmt.Println(inside)
}
