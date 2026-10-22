//! Port of csv.cpp.

use std::collections::BTreeMap;
use std::io::BufRead;

pub fn csv_split(s: &str) -> Vec<String> {
    todo!("csv::csv_split")
}

pub fn csv_dequote(s: String) -> String {
    todo!("csv::csv_dequote")
}

/// Reads one line including its newline, like the C++ loop over `getc`.
/// Returns an empty string at the end of the file.
pub fn csv_getline(f: &mut dyn BufRead) -> Vec<u8> {
    todo!("csv::csv_getline")
}

pub fn readcsv(fn_: &str, header: &mut Vec<String>, mapping: &mut BTreeMap<String, Vec<String>>) {
    todo!("csv::readcsv")
}

pub fn is_number(s: &str) -> bool {
    todo!("csv::is_number")
}
