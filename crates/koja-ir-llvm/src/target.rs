//! The triple, CPU, and feature set the emitted object code is built
//! for. One [`TargetSpec`] per compile feeds the
//! single `TargetMachine` that both type layout and object emission
//! use, so the two can never disagree.
//!
//! The default CPU is a portable baseline, not the build host.
//! Compiling for the host bakes its instruction set into the binary,
//! and a release built on an AVX-512 CI runner then dies with SIGILL
//! on any machine without it. Two builds of one commit must also
//! produce the same instruction set, which a host pick cannot promise.

use std::sync::Once;

use inkwell::OptimizationLevel;
use inkwell::llvm_sys::support::LLVMParseCommandLineOptions;
use inkwell::targets::{
    CodeModel, InitializationConfig, RelocMode, Target, TargetMachine, TargetTriple,
};

use crate::error::LlvmError;
use crate::reductions;

/// Default macOS deployment target baked into the emitted object's
/// triple when `MACOSX_DEPLOYMENT_TARGET` is unset. Matches
/// `koja-driver/src/link.rs::DEFAULT_MACOS_DEPLOYMENT_TARGET` and
/// the workspace `MACOSX_DEPLOYMENT_TARGET` so user binaries link
/// without `ld: warning: object file ... built for newer macOS
/// version` mismatches when the host SDK is newer than the floor.
#[cfg(target_os = "macos")]
const DEFAULT_MACOS_DEPLOYMENT_TARGET: &str = "11.0";

/// LLVM triple arch component for the macOS host architecture.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const MACOS_TARGET_ARCH: &str = "arm64";
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
const MACOS_TARGET_ARCH: &str = "x86_64";

/// Portable CPU model for the build architecture. The X86 backend
/// builds a fresh subtarget per function and indexes scheduling
/// tables that only exist for named models, so it needs a real name
/// rather than `generic`. `x86-64-v2` (SSE4.2 and POPCNT, no AVX)
/// has full tables and runs on every x86_64 machine still in
/// service. The AArch64 backend has no such gap, so `generic` is the
/// ARMv8.0 baseline there.
#[cfg(target_arch = "x86_64")]
const PORTABLE_CPU: &str = "x86-64-v2";
#[cfg(not(target_arch = "x86_64"))]
const PORTABLE_CPU: &str = "generic";

/// Which CPU the emitted code may assume.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TargetCpu {
    /// Every instruction the build machine supports. The binary is
    /// only guaranteed to run on that machine.
    Native,
    /// A baseline model for the build architecture. The binary runs
    /// on any machine of that architecture.
    #[default]
    Portable,
}

/// Triple, CPU, and feature string for one compile.
pub(crate) struct TargetSpec {
    cpu: String,
    features: String,
    triple: TargetTriple,
}

impl TargetSpec {
    pub(crate) fn new(target_cpu: TargetCpu) -> Self {
        let cpu = match target_cpu {
            TargetCpu::Native => TargetMachine::get_host_cpu_name().to_string(),
            TargetCpu::Portable => PORTABLE_CPU.to_string(),
        };
        Self {
            cpu,
            features: features_for(target_cpu),
            triple: host_triple(),
        }
    }

    /// Build the target machine every emission step shares.
    pub(crate) fn target_machine(
        &self,
        opt_level: OptimizationLevel,
    ) -> Result<TargetMachine, LlvmError> {
        initialize_llvm();
        let target = Target::from_triple(&self.triple)
            .map_err(|e| LlvmError::ObjectEmit(format!("failed to get target: {e}")))?;
        target
            .create_target_machine(
                &self.triple,
                &self.cpu,
                &self.features,
                opt_level,
                RelocMode::PIC,
                CodeModel::Default,
            )
            .ok_or_else(|| LlvmError::ObjectEmit("failed to create target machine".to_string()))
    }
}

/// Feature string for `target_cpu`. Portable builds carry only the
/// reduction-budget register reservation. Host features are what
/// leak `+avx512f` back in even under a baseline CPU name, so they
/// ride along for native builds only.
fn features_for(target_cpu: TargetCpu) -> String {
    let host = match target_cpu {
        TargetCpu::Native => TargetMachine::get_host_cpu_features().to_string(),
        TargetCpu::Portable => String::new(),
    };
    match reductions::budget_register_feature() {
        Some(reservation) if host.is_empty() => reservation.to_string(),
        Some(reservation) => format!("{host},{reservation}"),
        None => host,
    }
}

/// Returns the LLVM triple the emitted object file declares. On
/// macOS, pin the deployment-target portion (honoring
/// `MACOSX_DEPLOYMENT_TARGET` if the caller has set one, otherwise
/// [`DEFAULT_MACOS_DEPLOYMENT_TARGET`]) so the bundled crypto
/// archives and the user binary land on the same floor. Elsewhere,
/// fall back to whatever LLVM thinks the host is.
fn host_triple() -> TargetTriple {
    #[cfg(target_os = "macos")]
    {
        let version = std::env::var("MACOSX_DEPLOYMENT_TARGET")
            .unwrap_or_else(|_| DEFAULT_MACOS_DEPLOYMENT_TARGET.to_string());
        TargetTriple::create(&format!("{MACOS_TARGET_ARCH}-apple-macosx{version}"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        TargetMachine::get_default_triple()
    }
}

/// Register the native target and pin instruction selection to
/// SelectionDAG. Runs once per process, before the first target
/// machine is built.
fn initialize_llvm() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        Target::initialize_native(&InitializationConfig::default())
            .expect("LLVM emit: failed to initialize native target");
        force_selection_dag();
    });
}

/// Pin instruction selection to SelectionDAG (plus FastISel) for the
/// whole process. At `-O0` on aarch64 LLVM defaults to GlobalISel and
/// silently falls back to SelectionDAG for any function it cannot
/// select, which includes every function carrying the
/// reduction-budget register intrinsics from [`crate::reductions`].
/// The two selectors disagree on the stack placement of split
/// aggregate arguments (GlobalISel packs byte-sized pieces into
/// 1-byte slots, SelectionDAG into 4-byte slots), so a mixed-selector
/// module corrupts by-value aggregates like union payloads at call
/// boundaries. One selector for every function keeps callers and
/// callees in agreement.
fn force_selection_dag() {
    let args = [c"koja".as_ptr(), c"-global-isel=0".as_ptr()];
    unsafe { LLVMParseCommandLineOptions(args.len() as i32, args.as_ptr(), std::ptr::null()) };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The portable spec must not depend on the build host, so two
    /// builds of one commit emit the same instruction set.
    #[test]
    fn portable_spec_is_host_independent() {
        let spec = TargetSpec::new(TargetCpu::Portable);
        assert_eq!(spec.cpu, PORTABLE_CPU);
        assert_eq!(
            spec.features,
            reductions::budget_register_feature().unwrap_or_default()
        );
    }

    #[test]
    fn portable_spec_builds_a_target_machine() {
        let spec = TargetSpec::new(TargetCpu::Portable);
        assert!(spec.target_machine(OptimizationLevel::None).is_ok());
    }
}
