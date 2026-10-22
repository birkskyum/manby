//! Driver for differential tests of the libc++ `std::sort` port.
//! Reads lines "n k0 k1 ..." and prints the original indices after sorting on the keys only.

use std::io::{self, BufWriter, Read, Write};

#[derive(Clone, Copy)]
struct Node {
    key: i64,
    id: usize,
}

fn main() {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input).unwrap();
    let mut tokens = input.split_ascii_whitespace();
    let mut out = BufWriter::new(io::stdout().lock());
    while let Some(n) = tokens.next() {
        let n: usize = n.parse().unwrap();
        let mut v: Vec<Node> = (0..n)
            .map(|id| Node {
                key: tokens.next().unwrap().parse().unwrap(),
                id,
            })
            .collect();
        clipper2::libcxx_sort::sort(&mut v, |a, b| a.key < b.key);
        let ids: Vec<String> = v.iter().map(|x| x.id.to_string()).collect();
        writeln!(out, "{}", ids.join(" ")).unwrap();
    }
}
