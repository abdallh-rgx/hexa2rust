def fib(n):
    if n <= 1:
        return n
    return fib(n - 1) + fib(n - 2)

class Counter:
    def __init__(self, start=0):
        self.value = start
    def increment(self, by=1):
        self.value += by
        return self.value

def floordiv_demo(a, b):
    q = a // b
    r = a % b
    return q + r

squares = [x * x for x in range(10) if x % 2 == 0]
c = Counter()
c.increment(5)
print(fib(10), squares, c.value)
