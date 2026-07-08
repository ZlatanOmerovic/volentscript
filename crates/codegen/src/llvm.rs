//! The LLVM backend, via `inkwell` (pinned: LLVM 22 / feature `llvm22-1`).

/// LLVM implementor of [`crate::Backend`]. Real codegen lands in P3.
#[derive(Debug, Default)]
pub struct LlvmBackend {}

#[cfg(test)]
mod tests {
    use inkwell::context::Context;

    /// Smoke test: proves the inkwell → llvm-sys → brew LLVM 22 link works
    /// end to end, so P3 doesn't discover toolchain breakage late.
    #[test]
    fn inkwell_links_and_builds_a_module() {
        let context = Context::create();
        let module = context.create_module("smoke");
        let i32_type = context.i32_type();
        let fn_type = i32_type.fn_type(&[], false);
        let function = module.add_function("answer", fn_type, None);
        let entry = context.append_basic_block(function, "entry");
        let builder = context.create_builder();
        builder.position_at_end(entry);
        builder
            .build_return(Some(&i32_type.const_int(42, false)))
            .expect("build_return");
        assert!(function.verify(true), "LLVM function failed verification");
    }
}
