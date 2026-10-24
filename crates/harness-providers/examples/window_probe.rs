//! Throwaway probe: print the context window the catalog resolves for `provider\tmodel` rows on
//! stdin, alongside whatever the provider itself reported in the same row.
use std::io::Read;

fn main() {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).unwrap();
    for line in input.lines() {
        let mut fields = line.split('\t');
        let provider = fields.next().unwrap_or("");
        let model = fields.next().unwrap_or("");
        let reported = fields.next().unwrap_or("");
        let resolved = harness_providers::get_context_window(provider, model).unwrap_or(0);
        let flag = match reported.parse::<u32>() {
            Ok(real) if real != resolved => "  <-- MISMATCH",
            _ => "",
        };
        println!("{provider}\t{model}\t{resolved}\t{reported}{flag}");
    }
}
