fn main() {
    let text = "the quick brown fox jumps over the lazy dog";
    let mut checksum: i64 = 0;
    for _ in 0..60000 {
        let joined = text.split(' ').collect::<Vec<_>>().join("-");
        checksum += joined.find("fox").map(|i| i as i64).unwrap_or(-1);
        checksum += joined.to_uppercase().len() as i64;
        let replaced = joined.replacen("quick", "slow", 1);
        checksum += replaced.rfind('o').map(|i| i as i64).unwrap_or(-1);
    }
    println!("{checksum}");
}
