//! Math Host Functions
//!
//! Provides mathematical functions that don't have native WASM instructions.
//! - Trigonometric: sin, cos, tan, asin, acos, atan, atan2
//! - Hyperbolic: sinh, cosh, tanh
//! - Logarithmic/Exponential: ln, log10, log2, exp, exp2, pow, sqrt
//!
//! All functions are generic over `WasmStateCore` to work with any runtime.

use super::state::WasmStateCore;
use crate::error::BridgeResult;
use rand::prelude::*;
use wasmtime::{Caller, Linker};

/// Register all math functions with the linker
///
/// These are pure functions that don't access state, but we need the generic
/// parameter for wasmtime's Caller type.
pub fn register_functions<S: WasmStateCore>(linker: &mut Linker<S>) -> BridgeResult<()> {
    // =========================================
    // BASIC MATH OPERATIONS
    // =========================================

    // math_pow - Power function (base^exp)
    linker.func_wrap(
        "env",
        "math_pow",
        |_: Caller<'_, S>, base: f64, exp: f64| -> f64 {
            base.powf(exp)
        },
    )?;

    // math.pow - Dot notation alias
    linker.func_wrap(
        "env",
        "math.pow",
        |_: Caller<'_, S>, base: f64, exp: f64| -> f64 {
            base.powf(exp)
        },
    )?;

    // math_sqrt - Square root
    linker.func_wrap(
        "env",
        "math_sqrt",
        |_: Caller<'_, S>, x: f64| -> f64 {
            x.sqrt()
        },
    )?;

    // math.sqrt - Dot notation alias
    linker.func_wrap(
        "env",
        "math.sqrt",
        |_: Caller<'_, S>, x: f64| -> f64 {
            x.sqrt()
        },
    )?;

    // =========================================
    // TRIGONOMETRIC FUNCTIONS
    // =========================================

    // math_sin - Sine
    linker.func_wrap(
        "env",
        "math_sin",
        |_: Caller<'_, S>, x: f64| -> f64 { x.sin() },
    )?;

    linker.func_wrap(
        "env",
        "math.sin",
        |_: Caller<'_, S>, x: f64| -> f64 { x.sin() },
    )?;

    // math_cos - Cosine
    linker.func_wrap(
        "env",
        "math_cos",
        |_: Caller<'_, S>, x: f64| -> f64 { x.cos() },
    )?;

    linker.func_wrap(
        "env",
        "math.cos",
        |_: Caller<'_, S>, x: f64| -> f64 { x.cos() },
    )?;

    // math_tan - Tangent
    linker.func_wrap(
        "env",
        "math_tan",
        |_: Caller<'_, S>, x: f64| -> f64 { x.tan() },
    )?;

    linker.func_wrap(
        "env",
        "math.tan",
        |_: Caller<'_, S>, x: f64| -> f64 { x.tan() },
    )?;

    // =========================================
    // INVERSE TRIGONOMETRIC FUNCTIONS
    // =========================================

    // math_asin - Arc sine
    linker.func_wrap(
        "env",
        "math_asin",
        |_: Caller<'_, S>, x: f64| -> f64 { x.asin() },
    )?;

    linker.func_wrap(
        "env",
        "math.asin",
        |_: Caller<'_, S>, x: f64| -> f64 { x.asin() },
    )?;

    // math_acos - Arc cosine
    linker.func_wrap(
        "env",
        "math_acos",
        |_: Caller<'_, S>, x: f64| -> f64 { x.acos() },
    )?;

    linker.func_wrap(
        "env",
        "math.acos",
        |_: Caller<'_, S>, x: f64| -> f64 { x.acos() },
    )?;

    // math_atan - Arc tangent
    linker.func_wrap(
        "env",
        "math_atan",
        |_: Caller<'_, S>, x: f64| -> f64 { x.atan() },
    )?;

    linker.func_wrap(
        "env",
        "math.atan",
        |_: Caller<'_, S>, x: f64| -> f64 { x.atan() },
    )?;

    // math_atan2 - Two-argument arc tangent
    linker.func_wrap(
        "env",
        "math_atan2",
        |_: Caller<'_, S>, y: f64, x: f64| -> f64 { y.atan2(x) },
    )?;

    linker.func_wrap(
        "env",
        "math.atan2",
        |_: Caller<'_, S>, y: f64, x: f64| -> f64 { y.atan2(x) },
    )?;

    // =========================================
    // HYPERBOLIC FUNCTIONS
    // =========================================

    // math_sinh - Hyperbolic sine
    linker.func_wrap(
        "env",
        "math_sinh",
        |_: Caller<'_, S>, x: f64| -> f64 { x.sinh() },
    )?;

    linker.func_wrap(
        "env",
        "math.sinh",
        |_: Caller<'_, S>, x: f64| -> f64 { x.sinh() },
    )?;

    // math_cosh - Hyperbolic cosine
    linker.func_wrap(
        "env",
        "math_cosh",
        |_: Caller<'_, S>, x: f64| -> f64 { x.cosh() },
    )?;

    linker.func_wrap(
        "env",
        "math.cosh",
        |_: Caller<'_, S>, x: f64| -> f64 { x.cosh() },
    )?;

    // math_tanh - Hyperbolic tangent
    linker.func_wrap(
        "env",
        "math_tanh",
        |_: Caller<'_, S>, x: f64| -> f64 { x.tanh() },
    )?;

    linker.func_wrap(
        "env",
        "math.tanh",
        |_: Caller<'_, S>, x: f64| -> f64 { x.tanh() },
    )?;

    // =========================================
    // LOGARITHMIC FUNCTIONS
    // =========================================

    // math_ln - Natural logarithm
    linker.func_wrap(
        "env",
        "math_ln",
        |_: Caller<'_, S>, x: f64| -> f64 { x.ln() },
    )?;

    linker.func_wrap(
        "env",
        "math.ln",
        |_: Caller<'_, S>, x: f64| -> f64 { x.ln() },
    )?;

    // math_log10 - Base-10 logarithm
    linker.func_wrap(
        "env",
        "math_log10",
        |_: Caller<'_, S>, x: f64| -> f64 { x.log10() },
    )?;

    linker.func_wrap(
        "env",
        "math.log10",
        |_: Caller<'_, S>, x: f64| -> f64 { x.log10() },
    )?;

    // math_log2 - Base-2 logarithm
    linker.func_wrap(
        "env",
        "math_log2",
        |_: Caller<'_, S>, x: f64| -> f64 { x.log2() },
    )?;

    linker.func_wrap(
        "env",
        "math.log2",
        |_: Caller<'_, S>, x: f64| -> f64 { x.log2() },
    )?;

    // =========================================
    // EXPONENTIAL FUNCTIONS
    // =========================================

    // math_exp - e^x
    linker.func_wrap(
        "env",
        "math_exp",
        |_: Caller<'_, S>, x: f64| -> f64 { x.exp() },
    )?;

    linker.func_wrap(
        "env",
        "math.exp",
        |_: Caller<'_, S>, x: f64| -> f64 { x.exp() },
    )?;

    // math_exp2 - 2^x
    linker.func_wrap(
        "env",
        "math_exp2",
        |_: Caller<'_, S>, x: f64| -> f64 { x.exp2() },
    )?;

    linker.func_wrap(
        "env",
        "math.exp2",
        |_: Caller<'_, S>, x: f64| -> f64 { x.exp2() },
    )?;

    // =========================================
    // ROUNDING FUNCTIONS
    // =========================================

    // math_floor - Floor (round toward negative infinity)
    linker.func_wrap(
        "env",
        "math_floor",
        |_: Caller<'_, S>, x: f64| -> f64 { x.floor() },
    )?;

    linker.func_wrap(
        "env",
        "math.floor",
        |_: Caller<'_, S>, x: f64| -> f64 { x.floor() },
    )?;

    // math_ceil - Ceiling (round toward positive infinity)
    linker.func_wrap(
        "env",
        "math_ceil",
        |_: Caller<'_, S>, x: f64| -> f64 { x.ceil() },
    )?;

    linker.func_wrap(
        "env",
        "math.ceil",
        |_: Caller<'_, S>, x: f64| -> f64 { x.ceil() },
    )?;

    // math_round - Round to nearest integer
    linker.func_wrap(
        "env",
        "math_round",
        |_: Caller<'_, S>, x: f64| -> f64 { x.round() },
    )?;

    linker.func_wrap(
        "env",
        "math.round",
        |_: Caller<'_, S>, x: f64| -> f64 { x.round() },
    )?;

    // math_trunc - Truncate (round toward zero)
    linker.func_wrap(
        "env",
        "math_trunc",
        |_: Caller<'_, S>, x: f64| -> f64 { x.trunc() },
    )?;

    linker.func_wrap(
        "env",
        "math.trunc",
        |_: Caller<'_, S>, x: f64| -> f64 { x.trunc() },
    )?;

    // =========================================
    // UTILITY FUNCTIONS
    // =========================================

    // math_abs - Absolute value
    linker.func_wrap(
        "env",
        "math_abs",
        |_: Caller<'_, S>, x: f64| -> f64 { x.abs() },
    )?;

    linker.func_wrap(
        "env",
        "math.abs",
        |_: Caller<'_, S>, x: f64| -> f64 { x.abs() },
    )?;

    // math_min - Minimum of two values
    linker.func_wrap(
        "env",
        "math_min",
        |_: Caller<'_, S>, a: f64, b: f64| -> f64 { a.min(b) },
    )?;

    linker.func_wrap(
        "env",
        "math.min",
        |_: Caller<'_, S>, a: f64, b: f64| -> f64 { a.min(b) },
    )?;

    // math_max - Maximum of two values
    linker.func_wrap(
        "env",
        "math_max",
        |_: Caller<'_, S>, a: f64, b: f64| -> f64 { a.max(b) },
    )?;

    linker.func_wrap(
        "env",
        "math.max",
        |_: Caller<'_, S>, a: f64, b: f64| -> f64 { a.max(b) },
    )?;

    // math_sign - Sign of a number (-1.0, 0.0, or 1.0)
    linker.func_wrap(
        "env",
        "math_sign",
        |_: Caller<'_, S>, x: f64| -> f64 { x.signum() },
    )?;

    linker.func_wrap(
        "env",
        "math.sign",
        |_: Caller<'_, S>, x: f64| -> f64 { x.signum() },
    )?;

    // =========================================
    // CONSTANTS AND RANDOM
    // =========================================

    // math_pi - Return PI constant
    linker.func_wrap(
        "env",
        "math_pi",
        |_: Caller<'_, S>| -> f64 { std::f64::consts::PI },
    )?;

    linker.func_wrap(
        "env",
        "math.pi",
        |_: Caller<'_, S>| -> f64 { std::f64::consts::PI },
    )?;

    // math_e - Return Euler's number
    linker.func_wrap(
        "env",
        "math_e",
        |_: Caller<'_, S>| -> f64 { std::f64::consts::E },
    )?;

    linker.func_wrap(
        "env",
        "math.e",
        |_: Caller<'_, S>| -> f64 { std::f64::consts::E },
    )?;

    // math_random - Return random number between 0.0 and 1.0
    linker.func_wrap(
        "env",
        "math_random",
        |_: Caller<'_, S>| -> f64 { rand::thread_rng().gen::<f64>() },
    )?;

    linker.func_wrap(
        "env",
        "math.random",
        |_: Caller<'_, S>| -> f64 { rand::thread_rng().gen::<f64>() },
    )?;

    // =========================================
    // ADDITIONAL LOGARITHMS / EXPONENTIALS
    // =========================================

    // math_log - Alias for natural log (matches Math.log in JS)
    linker.func_wrap("env", "math_log", |_: Caller<'_, S>, x: f64| -> f64 { x.ln() })?;
    linker.func_wrap("env", "math.log", |_: Caller<'_, S>, x: f64| -> f64 { x.ln() })?;

    // math_cbrt - Cube root
    linker.func_wrap("env", "math_cbrt", |_: Caller<'_, S>, x: f64| -> f64 { x.cbrt() })?;
    linker.func_wrap("env", "math.cbrt", |_: Caller<'_, S>, x: f64| -> f64 { x.cbrt() })?;

    // math_log1p - ln(1 + x), accurate for small x
    linker.func_wrap("env", "math_log1p", |_: Caller<'_, S>, x: f64| -> f64 { x.ln_1p() })?;
    linker.func_wrap("env", "math.log1p", |_: Caller<'_, S>, x: f64| -> f64 { x.ln_1p() })?;

    // math_expm1 - exp(x) - 1, accurate for small x
    linker.func_wrap("env", "math_expm1", |_: Caller<'_, S>, x: f64| -> f64 { x.exp_m1() })?;
    linker.func_wrap("env", "math.expm1", |_: Caller<'_, S>, x: f64| -> f64 { x.exp_m1() })?;

    // =========================================
    // CLAMP / HYPOT / FMOD
    // =========================================

    // math_clamp - Clamp x to [min, max]
    linker.func_wrap("env", "math_clamp",
        |_: Caller<'_, S>, x: f64, min: f64, max: f64| -> f64 { x.max(min).min(max) })?;
    linker.func_wrap("env", "math.clamp",
        |_: Caller<'_, S>, x: f64, min: f64, max: f64| -> f64 { x.max(min).min(max) })?;

    // math_hypot - sqrt(a^2 + b^2)
    linker.func_wrap("env", "math_hypot",
        |_: Caller<'_, S>, a: f64, b: f64| -> f64 { a.hypot(b) })?;
    linker.func_wrap("env", "math.hypot",
        |_: Caller<'_, S>, a: f64, b: f64| -> f64 { a.hypot(b) })?;

    // math_fmod - Floating-point remainder
    linker.func_wrap("env", "math_fmod",
        |_: Caller<'_, S>, a: f64, b: f64| -> f64 { a % b })?;
    linker.func_wrap("env", "math.fmod",
        |_: Caller<'_, S>, a: f64, b: f64| -> f64 { a % b })?;

    // =========================================
    // MATH CONSTANTS
    // =========================================

    linker.func_wrap("env", "math_ln2",     |_: Caller<'_, S>| -> f64 { std::f64::consts::LN_2 })?;
    linker.func_wrap("env", "math.ln2",     |_: Caller<'_, S>| -> f64 { std::f64::consts::LN_2 })?;
    linker.func_wrap("env", "math_ln10",    |_: Caller<'_, S>| -> f64 { std::f64::consts::LN_10 })?;
    linker.func_wrap("env", "math.ln10",    |_: Caller<'_, S>| -> f64 { std::f64::consts::LN_10 })?;
    linker.func_wrap("env", "math_log2e",   |_: Caller<'_, S>| -> f64 { std::f64::consts::LOG2_E })?;
    linker.func_wrap("env", "math.log2e",   |_: Caller<'_, S>| -> f64 { std::f64::consts::LOG2_E })?;
    linker.func_wrap("env", "math_log10e",  |_: Caller<'_, S>| -> f64 { std::f64::consts::LOG10_E })?;
    linker.func_wrap("env", "math.log10e",  |_: Caller<'_, S>| -> f64 { std::f64::consts::LOG10_E })?;
    linker.func_wrap("env", "math_sqrt2",   |_: Caller<'_, S>| -> f64 { std::f64::consts::SQRT_2 })?;
    linker.func_wrap("env", "math.sqrt2",   |_: Caller<'_, S>| -> f64 { std::f64::consts::SQRT_2 })?;
    linker.func_wrap("env", "math_sqrt1_2", |_: Caller<'_, S>| -> f64 { std::f64::consts::FRAC_1_SQRT_2 })?;
    linker.func_wrap("env", "math.sqrt1_2", |_: Caller<'_, S>| -> f64 { std::f64::consts::FRAC_1_SQRT_2 })?;

    // =========================================
    // INTEGER MATH
    // =========================================

    linker.func_wrap("env", "math_abs_i32",
        |_: Caller<'_, S>, x: i32| -> i32 { x.wrapping_abs() })?;
    linker.func_wrap("env", "math.abs_i32",
        |_: Caller<'_, S>, x: i32| -> i32 { x.wrapping_abs() })?;

    linker.func_wrap("env", "math_min_i32",
        |_: Caller<'_, S>, a: i32, b: i32| -> i32 { a.min(b) })?;
    linker.func_wrap("env", "math.min_i32",
        |_: Caller<'_, S>, a: i32, b: i32| -> i32 { a.min(b) })?;

    linker.func_wrap("env", "math_max_i32",
        |_: Caller<'_, S>, a: i32, b: i32| -> i32 { a.max(b) })?;
    linker.func_wrap("env", "math.max_i32",
        |_: Caller<'_, S>, a: i32, b: i32| -> i32 { a.max(b) })?;

    // math_random_int - Inclusive integer in [min, max]
    linker.func_wrap("env", "math_random_int",
        |_: Caller<'_, S>, min: i32, max: i32| -> i32 {
            if min >= max { return min; }
            rand::thread_rng().gen_range(min..=max)
        })?;
    linker.func_wrap("env", "math.random_int",
        |_: Caller<'_, S>, min: i32, max: i32| -> i32 {
            if min >= max { return min; }
            rand::thread_rng().gen_range(min..=max)
        })?;

    // =========================================
    // NUMERIC PREDICATES
    // =========================================

    linker.func_wrap("env", "math_is_nan",
        |_: Caller<'_, S>, x: f64| -> i32 { if x.is_nan() { 1 } else { 0 } })?;
    linker.func_wrap("env", "math.is_nan",
        |_: Caller<'_, S>, x: f64| -> i32 { if x.is_nan() { 1 } else { 0 } })?;

    linker.func_wrap("env", "math_is_finite",
        |_: Caller<'_, S>, x: f64| -> i32 { if x.is_finite() { 1 } else { 0 } })?;
    linker.func_wrap("env", "math.is_finite",
        |_: Caller<'_, S>, x: f64| -> i32 { if x.is_finite() { 1 } else { 0 } })?;

    linker.func_wrap("env", "math_is_infinite",
        |_: Caller<'_, S>, x: f64| -> i32 { if x.is_infinite() { 1 } else { 0 } })?;
    linker.func_wrap("env", "math.is_infinite",
        |_: Caller<'_, S>, x: f64| -> i32 { if x.is_infinite() { 1 } else { 0 } })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_math_operations() {
        // Basic math operation tests
        assert!((std::f64::consts::PI.sin()).abs() < 1e-10);
        assert!((std::f64::consts::PI.cos() + 1.0).abs() < 1e-10);
        assert!((2.0_f64.powf(3.0) - 8.0).abs() < 1e-10);
        assert!((4.0_f64.sqrt() - 2.0).abs() < 1e-10);
    }
}
