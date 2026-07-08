const N: usize = 2500;

fn eval_a(i: usize, j: usize) -> f64 {
    let ij = i + j;
    1.0 / ((ij * (ij + 1)) / 2 + i + 1) as f64
}

fn multiply_av(v: &[f64], av: &mut [f64]) {
    for i in 0..N {
        let mut sum = 0.0;
        for j in 0..N {
            sum += eval_a(i, j) * v[j];
        }
        av[i] = sum;
    }
}

fn multiply_atv(v: &[f64], atv: &mut [f64]) {
    for i in 0..N {
        let mut sum = 0.0;
        for j in 0..N {
            sum += eval_a(j, i) * v[j];
        }
        atv[i] = sum;
    }
}

fn multiply_at_av(v: &[f64], atav: &mut [f64], tmp: &mut [f64]) {
    multiply_av(v, tmp);
    multiply_atv(tmp, atav);
}

fn main() {
    let mut u = vec![1.0f64; N];
    let mut v = vec![0.0f64; N];
    let mut tmp = vec![0.0f64; N];
    for _ in 0..10 {
        multiply_at_av(&u, &mut v, &mut tmp);
        multiply_at_av(&v, &mut u, &mut tmp);
    }
    let mut v_bv = 0.0;
    let mut vv = 0.0;
    for i in 0..N {
        v_bv += u[i] * v[i];
        vv += v[i] * v[i];
    }
    println!("{:.9}", (v_bv / vv).sqrt());
}
