fn main() {
    let nombres = vec!["ada", "grace", "linus"];
    // Error medido: la suma de longitudes es `usize`, no `String`.
    let total: String = nombres.iter().map(|n| n.len()).sum();
    println!("{} nombres, {} caracteres", nombres.len(), total);
}
