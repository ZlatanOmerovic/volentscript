struct Node { left: Option<Box<Node>>, right: Option<Box<Node>> }
fn make(depth: i32) -> Box<Node> {
    if depth == 0 { Box::new(Node { left: None, right: None }) }
    else { Box::new(Node { left: Some(make(depth - 1)), right: Some(make(depth - 1)) }) }
}
fn check(n: &Node) -> i32 {
    let mut total = 1;
    if let Some(l) = &n.left { total += check(l); }
    if let Some(r) = &n.right { total += check(r); }
    total
}
fn main() {
    let max_depth = 16;
    println!("stretch: {}", check(&make(max_depth + 1)));
    let long_lived = make(max_depth);
    let mut sum: i64 = 0;
    let mut d = 4;
    while d <= max_depth {
        let n = 1 << (max_depth - d + 4);
        for _ in 0..n { sum += check(&make(d)) as i64; }
        d += 2;
    }
    println!("sum: {sum}");
    println!("long: {}", check(&long_lived));
}
