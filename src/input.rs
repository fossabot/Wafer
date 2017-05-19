use csv;
use std::fs::create_dir;
use std::io::{Error, ErrorKind};
use std::path::Path;
use ndarray::{Array3, Zip};
use ndarray_parallel::prelude::*;
use grid;

/// Loads a wafefunction from a csv file on disk.
pub fn wavefunction_plain(wnum: u8) -> Result<Array3<f64>, csv::Error> {
    let filename = format!("./input/wavefunction_{}.csv", wnum);
    let filename_parital = format!("./input/wavefunction_{}_partial.csv", wnum);
    let file = if Path::new(&filename).exists() {
        Some(filename)
    } else if Path::new(&filename_parital).exists() {
        Some(filename_parital)
    } else {
        None
    };
    parse_csv_to_array3(file)
}

/// Loads a potential from a csv file on disk.
pub fn potential_plain() -> Result<Array3<f64>, csv::Error> {
    let filename = "./input/potential.csv";
    let file = if Path::new(&filename).exists() {
        Some(filename.to_string())
    } else {
        None
    };
    parse_csv_to_array3(file)
}

/// Checks that the folder `input` exists. If not, creates it.
/// This doesn't specifically need to happen for all instances,
/// but we may want to put restart values in there later on.
///
/// # Panics
/// * If directory can not be created. Gives `std::io::Error`.
pub fn check_input_dir() {
    if !Path::new("./input").exists() {
        let result = create_dir("./input");
        match result {
            Ok(_) => {}
            Err(err) => panic!("Cannot create input directory: {}", err),
        }
    }
}

fn parse_csv_to_array3(file: Option<String>) -> Result<Array3<f64>, csv::Error> {
    match file {
        Some(f) => {
            let mut rdr = csv::Reader::from_file(f)?.has_headers(false);
            let mut max_i = 0;
            let mut max_j = 0;
            let mut max_k = 0;
            let mut data: Vec<f64> = Vec::new();
            for record in rdr.decode() {
                let (i, j, k, value): (usize, usize, usize, f64) = record?;
                if i > max_i {
                    max_i = i
                };
                if j > max_j {
                    max_j = j
                };
                if k > max_k {
                    max_k = k
                };
                data.push(value);
            }
            let numx = max_i + 1;
            let numy = max_j + 1;
            let numz = max_k + 1;
            match Array3::<f64>::from_shape_vec((numx, numy, numz), data) {
                Ok(result) => {
                    //result is now a parsed Array3 with the work area inside.
                    //We must fill this into an array with CD boundaries.
                    let init_size: [usize; 3] = [numx + 6, numy + 6, numz + 6];
                    let mut complete = Array3::<f64>::zeros(init_size);
                    {
                        let mut work = grid::get_mut_work_area(&mut complete);
                        Zip::from(&mut work)
                            .and(result.view())
                            .par_apply(|work, &result| *work = result);
                    }
                    Ok(complete)
                }
                Err(err) => panic!("Error parsing file into array: {}", err),
            }
        }
        None => Err(csv::Error::Io(Error::from((ErrorKind::NotFound)))),
    }
}