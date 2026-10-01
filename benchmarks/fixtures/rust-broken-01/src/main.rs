fn main() {
    let total: String = suma(2, 3);
    println!("total: {total}");
}

/// Error medido: devuelve `i32` donde el llamador declaró `String`.
fn suma(a: i32, b: i32) -> i32 {
    a + b
}
