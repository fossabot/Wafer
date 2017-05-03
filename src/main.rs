include!(concat!(env!("OUT_DIR"), "/version.rs"));

fn main() {

    println!("                    ___");
    println!("   __      ____ _  / __\\__ _ __");
    println!("   \\ \\ /\\ / / _` |/ / / _ \\ '__|");
    println!("    \\ V  V / (_| / _\\|  __/ |");
    println!("     \\_/\\_/ \\__,/ /   \\___|_|    Current build SHA1: {}",
             sha());
    println!("              \\__/");
    println!("");
}

#[cfg(test)]
mod tests {
    #[test]
    fn placeholder() {
        let num = 5;
        assert_eq!(num, 5);
    }
}
