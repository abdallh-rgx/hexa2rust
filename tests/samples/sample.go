package main

import "fmt"

type Point struct {
	X int
	Y int
}

func (p Point) Manhattan() int {
	return p.X + p.Y
}

func sumTo(n int) int {
	total := 0
	for i := 1; i <= n; i++ {
		total += i
	}
	return total
}

func main() {
	p := Point{X: 3, Y: 4}
	nums := []int{1, 2, 3}
	total := 0
	for _, v := range nums {
		total += v
	}
	m := map[string]int{"a": 1, "b": 2}
	if v, ok := m["a"]; ok {
		total += v
	}
	fmt.Println(p.Manhattan(), sumTo(10), total)
}
