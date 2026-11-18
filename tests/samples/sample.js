const MAX = 100;
let count = 0;

function add(a, b = 2) { return a + b; }

class Point {
  constructor(x, y) { this.x = x; this.y = y; }
  norm() { return Math.sqrt(this.x * this.x + this.y * this.y); }
  static origin() { return new Point(0, 0); }
}

export function run(items) {
  let total = 0;
  for (const it of items) { total += it; }
  for (let i = 0; i < 10; i++) { count++; }
  const p = new Point(3, 4);
  if (p.norm() > 4) { return add(total, MAX); }
  return 0;
}
run([1, 2, 3]);
