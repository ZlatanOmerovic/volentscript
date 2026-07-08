package main

import (
	"fmt"
	"strings"
)

func main() {
	text := "the quick brown fox jumps over the lazy dog"
	checksum := 0
	for i := 0; i < 60000; i++ {
		joined := strings.Join(strings.Split(text, " "), "-")
		checksum += strings.Index(joined, "fox")
		checksum += len(strings.ToUpper(joined))
		replaced := strings.Replace(joined, "quick", "slow", 1)
		checksum += strings.LastIndex(replaced, "o")
	}
	fmt.Println(checksum)
}
