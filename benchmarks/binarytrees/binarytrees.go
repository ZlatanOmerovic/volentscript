package main

import "fmt"

type Node struct{ left, right *Node }

func make1(depth int) *Node {
	if depth == 0 {
		return &Node{}
	}
	return &Node{make1(depth - 1), make1(depth - 1)}
}

func check(n *Node) int {
	total := 1
	if n.left != nil {
		total += check(n.left)
	}
	if n.right != nil {
		total += check(n.right)
	}
	return total
}

func main() {
	maxDepth := 16
	fmt.Println("stretch:", check(make1(maxDepth+1)))
	longLived := make1(maxDepth)
	sum := 0
	for d := 4; d <= maxDepth; d += 2 {
		n := 1 << (maxDepth - d + 4)
		for i := 0; i < n; i++ {
			sum += check(make1(d))
		}
	}
	fmt.Println("sum:", sum)
	fmt.Println("long:", check(longLived))
}
