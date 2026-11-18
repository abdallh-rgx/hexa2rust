interface Shape { area(): number; name: string; }
enum Color { Red, Green = 5, Blue }

class Circle implements Shape {
    private readonly r: number;
    constructor(r: number) { this.r = r; }
    area(): number { return 3.14159 * this.r ** 2; }

}

function totalArea(shapes: Shape[]): number {
    let t = 0;
    for (const s of shapes) { t += s.area(); }
    return t;
}

const c = new Circle(2);
console.log(totalArea([c]), Color.Blue);
