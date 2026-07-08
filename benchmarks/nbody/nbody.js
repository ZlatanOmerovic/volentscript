class Body {
  constructor(x, y, z, vx, vy, vz, mass) {
    this.x = x; this.y = y; this.z = z;
    this.vx = vx; this.vy = vy; this.vz = vz; this.mass = mass;
  }
}
const PI = 3.141592653589793, SOLAR_MASS = 4 * PI * PI, DPY = 365.24;
const sun = new Body(0, 0, 0, 0, 0, 0, SOLAR_MASS);
const jupiter = new Body(4.84143144246472090, -1.16032004402742839, -0.103622044471123109,
  0.00166007664274403694 * DPY, 0.00769901118419740425 * DPY, -0.0000690460016972063023 * DPY,
  0.000954791938424326609 * SOLAR_MASS);
const saturn = new Body(8.34336671824457987, 4.12479856412430479, -0.403523417114321381,
  -0.00276742510726862411 * DPY, 0.00499852801234917238 * DPY, 0.0000230417297573763929 * DPY,
  0.000285885980666130812 * SOLAR_MASS);
const uranus = new Body(12.8943695621391310, -15.1111514016986312, -0.223307578892655734,
  0.00296460137564761618 * DPY, 0.00237847173959480950 * DPY, -0.0000296589568540237556 * DPY,
  0.0000436624404335156298 * SOLAR_MASS);
const neptune = new Body(15.3796971148509165, -25.9193146099879641, 0.179258772950371181,
  0.00268067772490389322 * DPY, 0.00162824170038242295 * DPY, -0.0000951592254519715870 * DPY,
  0.0000515138902046611451 * SOLAR_MASS);
const bodies = [sun, jupiter, saturn, uranus, neptune];

let px = 0, py = 0, pz = 0;
for (const b of bodies) { px += b.vx * b.mass; py += b.vy * b.mass; pz += b.vz * b.mass; }
sun.vx = -px / SOLAR_MASS; sun.vy = -py / SOLAR_MASS; sun.vz = -pz / SOLAR_MASS;

function energy() {
  let e = 0;
  for (let i = 0; i < 5; i++) {
    const b = bodies[i];
    e += 0.5 * b.mass * (b.vx * b.vx + b.vy * b.vy + b.vz * b.vz);
    for (let j = i + 1; j < 5; j++) {
      const b2 = bodies[j];
      const dx = b.x - b2.x, dy = b.y - b2.y, dz = b.z - b2.z;
      e -= (b.mass * b2.mass) / Math.sqrt(dx * dx + dy * dy + dz * dz);
    }
  }
  return e;
}
function advance(dt) {
  for (let i = 0; i < 5; i++) {
    const b = bodies[i];
    for (let j = i + 1; j < 5; j++) {
      const b2 = bodies[j];
      const dx = b.x - b2.x, dy = b.y - b2.y, dz = b.z - b2.z;
      const d2 = dx * dx + dy * dy + dz * dz;
      const mag = dt / (d2 * Math.sqrt(d2));
      b.vx -= dx * b2.mass * mag; b.vy -= dy * b2.mass * mag; b.vz -= dz * b2.mass * mag;
      b2.vx += dx * b.mass * mag; b2.vy += dy * b.mass * mag; b2.vz += dz * b.mass * mag;
    }
    b.x += dt * b.vx; b.y += dt * b.vy; b.z += dt * b.vz;
  }
}
console.log(energy().toFixed(9));
for (let s = 0; s < 5000000; s++) advance(0.01);
console.log(energy().toFixed(9));
