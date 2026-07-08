const size = 1500;
let inside = 0;
for (let py = 0; py < size; py++) {
  const ci = 2 * py / size - 1;
  for (let px = 0; px < size; px++) {
    const cr = 2 * px / size - 1.5;
    let zr = 0, zi = 0, k = 0;
    while (k < 50 && zr * zr + zi * zi <= 4) {
      const t = zr * zr - zi * zi + cr;
      zi = 2 * zr * zi + ci;
      zr = t;
      k++;
    }
    if (k === 50) inside++;
  }
}
console.log(inside);
