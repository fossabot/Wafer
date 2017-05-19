use ndarray::{Array3, ArrayView3, ArrayViewMut3, Zip};
use ndarray_parallel::prelude::*;
use slog::Logger;
use std::f64::MAX;
use config;
use config::{Config, Grid, Index3, PotentialType};
use potential;
use output;

#[derive(Debug)]
pub struct Potentials {
    pub v: Array3<f64>,
    a: Array3<f64>,
    b: Array3<f64>,
    epsilon: f64,
}

/// Parameters generated by the initialisation routine, potentials and wavefunction (phi).
#[derive(Debug)]
struct Params<'a, 'b> {
    potentials: &'b Potentials,
    phi: &'a mut Array3<f64>,
}

#[derive(Debug)]
pub struct Observables {
    pub energy: f64,
    pub norm2: f64,
    pub v_infinity: f64,
    pub r2: f64,
}

fn load_potential_arrays(config: &Config, log: &Logger) -> Potentials {
    info!(log, "Loading potential arrays");
    let mut minima: f64 = MAX;

    let result = match config.potential {
        PotentialType::FromFile => potential::from_file(),
        PotentialType::FromScript => potential::from_script(),
        _ => potential::generate(config),
    };
    let v: Array3<f64> = match result {
        Ok(r) => r,
        Err(err) => panic!("Error: {}", err),
    };


    let b = 1. / (1. + config.grid.dt * &v / 2.);
    let a = (1. - config.grid.dt * &v / 2.) * &b;

    // We can't do this in a par.
    // AFAIK, this is the safest way to work with the float here.
    for el in v.iter() {
        if el.is_finite() {
            minima = minima.min(*el);
        }
    }
    //Get 2*abs(min(potential)) for offset of beta
    let epsilon = 2. * minima.abs();

    if config.output.save_potential {
        info!(log, "Saving potential to disk");
        //Not sure if we should use someting like messagepack as there are matlab
        //and python bindings, or try for hdf5. The rust bindings there are pretty
        //shonky. So not sure. We'll need a text only option anyhow, so build that fist.
        match output::potential_plain(&v) {
            Ok(_) => {}
            Err(err) => crit!(log, "Could not write potential to disk: {}", err),
        }
    }

    Potentials {
        v: v,
        a: a,
        b: b,
        epsilon: epsilon,
    }
}

/// Runs the calculation and holds long term (system time) wavefunction storage
pub fn run(config: &Config, log: &Logger) {
    let potentials = load_potential_arrays(config, log);

    let mut w_store: Vec<Array3<f64>> = Vec::new();
    for wnum in config.wavenum..config.wavemax + 1 {
        //TODO: This error probably isn't the best way of handling this situation.
        match solve(config, log, &potentials, wnum, &w_store) {
            Some(w) => w_store.push(w),
            None => {
                panic!("Wavefunction is not converged. Cannot continue until convergence is \
                        reached.")
            }
        }
        //reInitSolver()
    }
    // done with main calculation.
    // solve finalise
}

/// Runs the actual computation once system is setup and ready.
fn solve(config: &Config,
         log: &Logger,
         pots: &Potentials,
         wnum: u8,
         w_store: &Vec<Array3<f64>>)
         -> Option<Array3<f64>> {

    // Initial conditions from config file if ground state,
    // but start from previously converged wfn if we're an excited state.
    // NOTE: This may not alwans be the sane choice. If we have a converged
    // low resolution version on file we'll want that instead
    let mut params = Params {
        potentials: pots,
        phi: &mut if wnum > 0 {
                      w_store[wnum as usize - 1].clone()
                  } else {
                      config::set_initial_conditions(config, log)
                  },
    };

    output::print_observable_header(wnum);

    let mut step = 0;
    let mut done = false;
    let mut converged = false;
    let mut last_energy = MAX; //std::f64::MAX
    let mut display_energy = MAX;
    while !done {

        let observables = compute_observables(config, &params);
        let norm_energy = observables.energy / observables.norm2;
        // Orthoganalise wavefunction
        if wnum > 0 {
            normalise_wavefunction(params.phi, observables.norm2);
            orthogonalise_wavefunction(wnum, params.phi, w_store);
        }
        //NOTE: Need to do a floating point comparison here if we want steps to be more than 2^64 (~1e19)
        // But I think it's just best to not have this option. 1e19 max.
        if step % config.output.snap_update == 0 {
            //TODO: I think we can do away with SNAPUPDATE now. Kill this if.
            config::symmetrise_wavefunction(config, params.phi);
            normalise_wavefunction(params.phi, observables.norm2);

            if (norm_energy - last_energy).abs() < config.tolerance {
                output::summary(&observables, wnum, config.grid.size.x as f64);
                converged = true;
                break;
            } else {
                display_energy = last_energy;
                last_energy = norm_energy;
            }
        }
        let tau = (step as f64) * config.grid.dt;
        let diff = (display_energy - norm_energy).abs();
        output::measurements(tau, diff, &observables);
        if step < config.max_steps {
            evolve(wnum, config, &mut params, w_store);
        }
        step += config.output.screen_update;
        done = step > config.max_steps;
    }

    if config.output.save_wavefns {
        //NOTE: This wil save regardless of whether it is converged or not, so we flag it if that's the case.
        info!(log, "Saving wavefunction {} to disk", wnum);
        match output::wavefunction_plain(&params.phi, wnum, converged) {
            Ok(_) => {}
            Err(err) => crit!(log, "Could not write wavefunction to disk: {}", err),
        }
    }

    if converged {
        info!(log, "Caluculation Converged");
        Some(params.phi.clone())
    } else {
        warn!(log, "Caluculation stopped due to maximum step limit.");
        None
    }
}

/// Computes observable values of the system, for example the energy
fn compute_observables(config: &Config, params: &Params) -> Observables {
    let energy = wfnc_energy(config, params);
    let work = get_work_area(params.phi);
    let norm2 = get_norm_squared(&work);
    let v_infinity = get_v_infinity_expectation_value(&work, config);
    let r2 = get_r_squared_expectation_value(&work, &config.grid);

    Observables {
        energy: energy,
        norm2: norm2,
        v_infinity: v_infinity,
        r2: r2,
    }
}

/// Normalisation of wavefunction
fn get_norm_squared(w: &ArrayView3<f64>) -> f64 {
    //NOTE: No complex conjugation due to all real input for now
    w.into_par_iter().map(|&el| el * el).sum()
}

/// Get v infinity
fn get_v_infinity_expectation_value(w: &ArrayView3<f64>, config: &Config) -> f64 {
    //NOTE: No complex conjugation due to all real input for now
    let mut work = Array3::<f64>::zeros(w.dim());
    Zip::indexed(&mut work)
        .and(w)
        .par_apply(|(i, j, k), work, &w| {
                       let idx = Index3 { x: i, y: j, z: k };
                       let potsub = match potential::potential_sub(config, &idx) {
                           Ok(p) => p,
                           Err(err) => panic!("Error: {}", err),
                       };
                       *work = w * w * potsub;
                   });
    work.scalar_sum()
}

/// Get r2
fn get_r_squared_expectation_value(w: &ArrayView3<f64>, grid: &Grid) -> f64 {
    //NOTE: No complex conjugation due to all real input for now
    let mut work = Array3::<f64>::zeros(w.dim());
    Zip::indexed(&mut work)
        .and(w)
        .par_apply(|(i, j, k), work, &w| {
                       let idx = Index3 { x: i, y: j, z: k };
                       let r2 = potential::calculate_r2(&idx, grid);
                       *work = w * w * r2;
                   });
    work.scalar_sum()
}

/// Gets energy of the corresponding wavefunction
//TODO: We can probably drop the config requirement and replace it with a grid modifier of dn*mass
fn wfnc_energy(config: &Config, params: &Params) -> f64 {

    let w = get_work_area(params.phi);
    let v = get_work_area(&params.potentials.v);

    // Simplify what we can here.
    let denominator = 360. * config.grid.dn.powi(2) * config.mass;

    let mut work = Array3::<f64>::zeros(w.dim());
    //NOTE: TODO: We don't have any complex conjugation here.
    // Complete matrix multiplication step using 7 point central differenc
    // TODO: Option for 3 or 5 point caclulation
    Zip::indexed(&mut work)
        .and(v)
        .and(w)
        .par_apply(|(i, j, k), work, &v, &w| {
            // Offset indexes as we are already in a slice
            let lx = i as isize + 3;
            let ly = j as isize + 3;
            let lz = k as isize + 3;
            let o = 3;
            // get a slice which gives us our matrix of central difference points
            let l = params
                .phi
                .slice(s![lx - 3..lx + 4, ly - 3..ly + 4, lz - 3..lz + 4]);
            // l can now be indexed with local offset `o` and modifiers
            *work = v * w * w -
                    w *
                    (2. * l[[o + 3, o, o]] - 27. * l[[o + 2, o, o]] + 270. * l[[o + 1, o, o]] +
                     270. * l[[o - 1, o, o]] -
                     27. * l[[o - 2, o, o]] + 2. * l[[o - 3, o, o]] +
                     2. * l[[o, o + 3, o]] - 27. * l[[o, o + 2, o]] +
                     270. * l[[o, o + 1, o]] +
                     270. * l[[o, o - 1, o]] -
                     27. * l[[o, o - 2, o]] + 2. * l[[o, o - 3, o]] +
                     2. * l[[o, o, o + 3]] - 27. * l[[o, o, o + 2]] +
                     270. * l[[o, o, o + 1]] +
                     270. * l[[o, o, o - 1]] -
                     27. * l[[o, o, o - 2]] + 2. * l[[o, o, o - 3]] -
                     1470. * w) / denominator;
        });
    // Sum result for total energy.
    work.scalar_sum()
}

/// Normalisation of the wavefunction
fn normalise_wavefunction(w: &mut Array3<f64>, norm2: f64) {
    //TODO: This can be moved directly into the calculation for now. It's only here due to normalisationCollect
    let norm = norm2.sqrt();
    w.par_map_inplace(|el| *el /= norm);
}

/// Uses Gram Schmidt orthogonalisation to identify the next excited state's wavefunction, even if it's degenerate
fn orthogonalise_wavefunction(wnum: u8, w: &mut Array3<f64>, w_store: &Vec<Array3<f64>>) {
    for idx in 0..wnum as usize {
        let lower = &w_store[idx];
        let overlap = (lower * &w.view()).scalar_sum(); //TODO: par this multiplication if possible. A temp work array and par_applied zip is slower, even with an unassigned array
        Zip::from(w.view_mut())
            .and(lower)
            .par_apply(|w, &lower| *w -= lower * overlap);
    }
}

fn get_work_area(w: &Array3<f64>) -> ArrayView3<f64> {
    // TODO: This is hardcoded to a 7 point stencil
    let dims = w.dim();
    w.slice(s![3..(dims.0 as isize) - 3,
               3..(dims.1 as isize) - 3,
               3..(dims.2 as isize) - 3])
}

fn get_mut_work_area(w: &mut Array3<f64>) -> ArrayViewMut3<f64> {
    // TODO: This is hardcoded to a 7 point stencil
    let dims = w.dim();
    w.slice_mut(s![3..(dims.0 as isize) - 3,
                   3..(dims.1 as isize) - 3,
                   3..(dims.2 as isize) - 3])
}

/// Evolves the solution a number of `steps`
fn evolve(wnum: u8, config: &Config, params: &mut Params, w_store: &Vec<Array3<f64>>) {
    //without mpi, this is just update interior (which is really updaterule if we dont need W)

    let mut work_dims = params.phi.dim();
    work_dims.0 -= 6;
    work_dims.1 -= 6;
    work_dims.2 -= 6;
    let mut steps = 0;
    loop {

        let mut work = Array3::<f64>::zeros(work_dims);
        {
            let w = get_work_area(params.phi);
            let a = get_work_area(&params.potentials.a);
            let b = get_work_area(&params.potentials.b);

            let denominator = 360. * config.grid.dn.powi(2) * config.mass;

            //NOTE: TODO: We don't have any complex conjugation here.
            // Complete matrix multiplication step using 7 point central difference
            // TODO: Option for 3 or 5 point caclulation
            Zip::indexed(&mut work)
                .and(a)
                .and(b)
                .and(w)
                .par_apply(|(i, j, k), work, &a, &b, &w| {
                    // Offset indexes as we are already in a slice
                    let lx = i as isize + 3;
                    let ly = j as isize + 3;
                    let lz = k as isize + 3;
                    let o = 3;
                    // get a slice which gives us our matrix of central difference points
                    let l = params
                        .phi
                        .slice(s![lx - 3..lx + 4, ly - 3..ly + 4, lz - 3..lz + 4]);
                    // l can now be indexed with local offset `o` and modifiers
                    *work =
                        w * a +
                        b * config.grid.dt *
                        (2. * l[[o + 3, o, o]] - 27. * l[[o + 2, o, o]] + 270. * l[[o + 1, o, o]] +
                         270. * l[[o - 1, o, o]] - 27. * l[[o - 2, o, o]] +
                         2. * l[[o - 3, o, o]] + 2. * l[[o, o + 3, o]] -
                         27. * l[[o, o + 2, o]] + 270. * l[[o, o + 1, o]] +
                         270. * l[[o, o - 1, o]] - 27. * l[[o, o - 2, o]] +
                         2. * l[[o, o - 3, o]] + 2. * l[[o, o, o + 3]] -
                         27. * l[[o, o, o + 2]] + 270. * l[[o, o, o + 1]] +
                         270. * l[[o, o, o - 1]] - 27. * l[[o, o, o - 2]] +
                         2. * l[[o, o, o - 3]] - 1470. * w) / denominator;
                });
        }
        {
            let mut w_fill = get_mut_work_area(params.phi);
            Zip::from(&mut w_fill)
                .and(&work)
                .par_apply(|w_fill, &work| { *w_fill = work; });
        }
        if wnum > 0 {
            let norm2: f64;
            {
                let work = get_work_area(params.phi);
                norm2 = get_norm_squared(&work);
            }
            normalise_wavefunction(params.phi, norm2);
            orthogonalise_wavefunction(wnum, params.phi, w_store);
        }
        steps += 1;
        if steps >= config.output.screen_update {
            break;
        }
    }
}
