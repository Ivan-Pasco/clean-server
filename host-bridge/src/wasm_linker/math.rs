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
