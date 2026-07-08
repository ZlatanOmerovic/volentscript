#[derive(Clone, Copy)]
struct Body { x: f64, y: f64, z: f64, vx: f64, vy: f64, vz: f64, mass: f64 }
const PI: f64 = 3.141592653589793;
const SM: f64 = 4.0 * PI * PI;
const DPY: f64 = 365.24;
fn energy(b: &[Body; 5]) -> f64 {
    let mut e = 0.0;
    for i in 0..5 {
        e += 0.5 * b[i].mass * (b[i].vx * b[i].vx + b[i].vy * b[i].vy + b[i].vz * b[i].vz);
        for j in i + 1..5 {
            let (dx, dy, dz) = (b[i].x - b[j].x, b[i].y - b[j].y, b[i].z - b[j].z);
            e -= (b[i].mass * b[j].mass) / (dx * dx + dy * dy + dz * dz).sqrt();
        }
    }
    e
}
fn advance(b: &mut [Body; 5], dt: f64) {
    for i in 0..5 {
        for j in i + 1..5 {
            let (dx, dy, dz) = (b[i].x - b[j].x, b[i].y - b[j].y, b[i].z - b[j].z);
            let d2 = dx * dx + dy * dy + dz * dz;
            let mag = dt / (d2 * d2.sqrt());
            let (mi, mj) = (b[i].mass, b[j].mass);
            b[i].vx -= dx * mj * mag; b[i].vy -= dy * mj * mag; b[i].vz -= dz * mj * mag;
            b[j].vx += dx * mi * mag; b[j].vy += dy * mi * mag; b[j].vz += dz * mi * mag;
        }
        b[i].x += dt * b[i].vx; b[i].y += dt * b[i].vy; b[i].z += dt * b[i].vz;
    }
}
fn main() {
    let mut b = [
        Body { x: 0.0, y: 0.0, z: 0.0, vx: 0.0, vy: 0.0, vz: 0.0, mass: SM },
        Body { x: 4.84143144246472090, y: -1.16032004402742839, z: -0.103622044471123109,
               vx: 0.00166007664274403694 * DPY, vy: 0.00769901118419740425 * DPY,
               vz: -0.0000690460016972063023 * DPY, mass: 0.000954791938424326609 * SM },
        Body { x: 8.34336671824457987, y: 4.12479856412430479, z: -0.403523417114321381,
               vx: -0.00276742510726862411 * DPY, vy: 0.00499852801234917238 * DPY,
               vz: 0.0000230417297573763929 * DPY, mass: 0.000285885980666130812 * SM },
        Body { x: 12.8943695621391310, y: -15.1111514016986312, z: -0.223307578892655734,
               vx: 0.00296460137564761618 * DPY, vy: 0.00237847173959480950 * DPY,
               vz: -0.0000296589568540237556 * DPY, mass: 0.0000436624404335156298 * SM },
        Body { x: 15.3796971148509165, y: -25.9193146099879641, z: 0.179258772950371181,
               vx: 0.00268067772490389322 * DPY, vy: 0.00162824170038242295 * DPY,
               vz: -0.0000951592254519715870 * DPY, mass: 0.0000515138902046611451 * SM },
    ];
    let (mut px, mut py, mut pz) = (0.0, 0.0, 0.0);
    for x in &b { px += x.vx * x.mass; py += x.vy * x.mass; pz += x.vz * x.mass; }
    b[0].vx = -px / SM; b[0].vy = -py / SM; b[0].vz = -pz / SM;
    println!("{:.9}", energy(&b));
    for _ in 0..5_000_000 { advance(&mut b, 0.01); }
    println!("{:.9}", energy(&b));
}
