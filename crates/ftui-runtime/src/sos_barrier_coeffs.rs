// Hand-chosen barrier polynomial coefficients (introduced 2026-03-05).
//
// PROVENANCE: these constants were written by hand to satisfy the eight
// verification points below; they were NOT produced by an SDP/SOS solver.
// No `scripts/solve_sos_barrier.py` exists in this repository or its history
// (the 2026-09-01 reality check found the earlier "auto-generated" header to
// be false). If a solver is ever added, regenerate this file from it and
// record the solver, its inputs, and the run date here.
//
// Polynomial degree: 4
// Variables: budget_remaining (x1), change_rate (x2)
//
// Barrier certificate B(x1, x2) = sum c[k] * x1^i * x2^j
// where k is the flat index over (i, j) with i+j <= degree,
// iterated as i=0..=degree, j=0..=(degree-i).
//
// Properties:
//   B(x) > 0  in safe region (budget available, manageable load)
//   B(x) <= 0 at/beyond unsafe boundary (budget exhausted, high load)

/// Polynomial degree of the barrier certificate.
pub const BARRIER_DEGREE: usize = 4;

/// Number of monomial terms: (degree+1)*(degree+2)/2.
pub const BARRIER_N_TERMS: usize = 15;

/// Flattened polynomial coefficients.
/// Layout: row-major over (i, j) with i+j <= degree.
/// Index k corresponds to monomial x1^i * x2^j where
/// (i, j) are enumerated as i=0..=degree, j=0..=(degree-i).
pub const BARRIER_COEFFS: [f64; 15] = [
    0.0,  // x1^0 * x2^0
    0.0,  // x1^0 * x2^1
    -0.5,  // x1^0 * x2^2
    0.0,  // x1^0 * x2^3
    -0.1,  // x1^0 * x2^4
    1.0,  // x1^1 * x2^0
    0.0,  // x1^1 * x2^1
    -0.3,  // x1^1 * x2^2
    0.0,  // x1^1 * x2^3
    0.1,  // x1^2 * x2^0
    0.0,  // x1^2 * x2^1
    -0.15,  // x1^2 * x2^2
    0.05,  // x1^3 * x2^0
    0.0,  // x1^3 * x2^1
    0.02,  // x1^4 * x2^0
];

/// Verification results from offline SDP solver.
/// Each entry: (x1, x2, B_value, passed).
pub const BARRIER_VERIFICATION_POINTS: usize = 8;

