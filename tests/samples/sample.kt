package demo

fun sumTo(n: Int): Int {
    var total = 0
    for (i in 1..n) {
        total += i
    }
    return total
}

data class Point(val x: Int, val y: Int) {
    fun manhattan(): Int = x + y
}

fun describe(p: Point): String {
    return if (p.x > 0) "right" else if (p.x < 0) "left" else "origin"
}

fun main() {
    val p = Point(3, 4)
    println("sum=${sumTo(10)} manhattan=${p.manhattan()} ${describe(p)}")
}
