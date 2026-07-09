//! The LLVM backend, via `inkwell` (pinned: LLVM 22 / feature `llvm22-1`).
//!
//! Value mapping (must stay in sync with the runtime's `ffi` module):
//! int/uint → `i32`, Number → `f64`, Boolean → `i1` (internal only; `u32`
//! at the C boundary), String → opaque `ptr` (null = AS3 null), `*` →
//! `{ i32, i64 }` boxes held in entry-block allocas and passed to the
//! runtime **by pointer** (aggregate by-value C ABIs are not replicated by
//! hand-built IR — pointers sidestep that).

use diagnostics::{Diagnostic, ErrorCode};
use inkwell::basic_block::BasicBlock;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine, TargetTriple,
};
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum, StructType};
use inkwell::values::{
    BasicMetadataValueEnum, BasicValueEnum, FloatValue, FunctionValue, IntValue, PointerValue,
};
use inkwell::{AddressSpace, FloatPredicate, IntPredicate, OptimizationLevel};
use mir::{BinOp, Builtin, Conv, ExprKind, NumMethod, Stmt, StrMethod, Ty, UnOp};

use crate::{Backend, CodegenOpts, ObjectFile};

/// Runtime type tags — ABI with `runtime::any::Tag`.
mod tag {
    pub const NULL: u32 = 1;
    pub const BOOLEAN: u32 = 2;
    pub const INT: u32 = 3;
    pub const UINT: u32 = 4;
    pub const NUMBER: u32 = 5;
    pub const STRING: u32 = 6;
    pub const OBJECT: u32 = 7;
    pub const ARRAY: u32 = 8;
    pub const VECTOR: u32 = 9;
    pub const FUNCTION: u32 = 10;
    pub const REGEXP: u32 = 11;
    pub const DATE: u32 = 12;
    pub const SOCKET: u32 = 13;
    pub const NAMESPACE: u32 = 14;
}

/// LLVM implementor of [`Backend`].
#[derive(Debug, Default)]
pub struct LlvmBackend {}

impl Backend for LlvmBackend {
    fn compile(
        &self,
        program: &mir::Program,
        opts: &CodegenOpts,
    ) -> Result<ObjectFile, Vec<Diagnostic>> {
        let context = Context::create();
        let module = context.create_module("volent");
        let machine = target_machine(opts).map_err(|m| {
            vec![Diagnostic::error(
                ErrorCode::NOT_IMPLEMENTED,
                format!("cannot initialize LLVM target: {m}"),
            )]
        })?;
        module.set_triple(&machine.get_triple());
        // The optimization pipeline is layout-sensitive; without the
        // target's data layout the passes assume a default that mismatches
        // the emitted object (miscompiles at O1+).
        module.set_data_layout(&machine.get_target_data().get_data_layout());

        let windows = machine
            .get_triple()
            .as_str()
            .to_string_lossy()
            .contains("windows");
        let mut cx = Cx {
            context: &context,
            module,
            builder: context.create_builder(),
            windows,
            any_ty: context.struct_type(
                &[context.i32_type().into(), context.i64_type().into()],
                false,
            ),
            classes: Vec::new(),
            wrappers: Default::default(),
            vwrappers: Default::default(),
            bwrappers: Default::default(),
        };

        // Declare every function first (mutual recursion), then emit bodies.
        let fns: Vec<FunctionValue> = program
            .functions
            .iter()
            .enumerate()
            .map(|(i, f)| cx.declare_function(i, f))
            .collect();
        cx.classes = build_class_artifacts(&cx, program, &fns, &machine.get_target_data());
        emit_ns_member_tables(&cx, program, &fns, &machine.get_target_data());
        for (f, decl) in program.functions.iter().zip(&fns) {
            FnCx::emit(&cx, &fns, program, f, *decl);
        }

        if std::env::var_os("VS_DUMP_IR").is_some() {
            eprintln!("{}", cx.module.print_to_string().to_string());
        }
        if let Err(e) = cx.module.verify() {
            // A verifier failure is a compiler bug, not a user error — but
            // never panic on the user (CLAUDE.md §4): report it.
            return Err(vec![Diagnostic::error(
                ErrorCode::NOT_IMPLEMENTED,
                format!("internal codegen error (LLVM verifier): {}", e.to_string()),
            )]);
        }
        // Optimization pipeline (new pass manager, SPECS §8 P13). Sound
        // with the conservative GC: vs_gc_safepoint is an opaque external
        // call, so LLVM keeps live GC pointers in callee-saved registers
        // or stack slots across it — both scanned at collection — and the
        // collector already pins blocks through interior pointers, which
        // covers GEP-derived addresses that outlive their base.
        if let Err(e) = cx.module.run_passes(
            opts.opt.pipeline(),
            &machine,
            inkwell::passes::PassBuilderOptions::create(),
        ) {
            return Err(vec![Diagnostic::error(
                ErrorCode::NOT_IMPLEMENTED,
                format!("internal codegen error (pass pipeline): {}", e.to_string()),
            )]);
        }
        if std::env::var_os("VS_DUMP_IR_OPT").is_some() {
            eprintln!("{}", cx.module.print_to_string().to_string());
        }
        let buffer = machine
            .write_to_memory_buffer(&cx.module, FileType::Object)
            .map_err(|e| {
                vec![Diagnostic::error(
                    ErrorCode::NOT_IMPLEMENTED,
                    format!("object emission failed: {e}"),
                )]
            })?;
        Ok(ObjectFile {
            bytes: buffer.as_slice().to_vec(),
        })
    }
}

fn target_machine(opts: &CodegenOpts) -> Result<TargetMachine, String> {
    Target::initialize_all(&InitializationConfig::default());
    let triple = match &opts.target_triple {
        Some(t) => TargetTriple::create(t),
        None => TargetMachine::get_default_triple(),
    };
    let target = Target::from_triple(&triple).map_err(|e| e.to_string())?;
    target
        .create_target_machine(
            &triple,
            "generic",
            "",
            match opts.opt {
                crate::OptLevel::O0 => OptimizationLevel::None,
                crate::OptLevel::O1 => OptimizationLevel::Less,
                crate::OptLevel::O2 => OptimizationLevel::Default,
                crate::OptLevel::O3 => OptimizationLevel::Aggressive,
            },
            RelocMode::PIC,
            CodeModel::Default,
        )
        .ok_or_else(|| format!("no target machine for {}", triple))
}

/// Module-level context shared by all functions.
struct Cx<'ctx> {
    context: &'ctx Context,
    module: Module<'ctx>,
    builder: Builder<'ctx>,
    any_ty: StructType<'ctx>,
    /// Windows target: the setjmp ABI differs (see emit_try).
    windows: bool,
    /// Per-class artifacts (RTTI globals, layouts, statics); index = class.
    classes: Vec<ClassArt<'ctx>>,
    /// Boxed-ABI wrappers for functions used as values (lazy, memoized).
    wrappers: std::cell::RefCell<std::collections::HashMap<u32, FunctionValue<'ctx>>>,
    /// Virtual-dispatch wrappers for bound methods, keyed (class, vslot).
    vwrappers: std::cell::RefCell<std::collections::HashMap<(u32, usize), FunctionValue<'ctx>>>,
    /// Builtin wrappers keyed by discriminant.
    bwrappers: std::cell::RefCell<std::collections::HashMap<u32, FunctionValue<'ctx>>>,
}

/// Per-class codegen artifacts.
struct ClassArt<'ctx> {
    /// The descriptor/RTTI global (VsClassDesc prefix + vtable — layout
    /// contract with runtime/src/object.rs).
    rtti: PointerValue<'ctx>,
    /// Its struct type (for vtable geps).
    rtti_ty: StructType<'ctx>,
    /// Instance layout: { descriptor ptr, slots... }.
    instance_ty: StructType<'ctx>,
    /// ABI size of an instance in bytes.
    size: u64,
    /// Expando slot byte offset (u32::MAX = sealed).
    expando_off: u32,
    /// Static field globals.
    statics: Vec<PointerValue<'ctx>>,
}

/// A computed value.
#[derive(Clone, Copy)]
enum Val<'ctx> {
    Int(IntValue<'ctx>),
    UInt(IntValue<'ctx>),
    Num(FloatValue<'ctx>),
    Bool(IntValue<'ctx>),
    /// String pointer (possibly null).
    Str(PointerValue<'ctx>),
    /// Class instance pointer (possibly null).
    Obj(PointerValue<'ctx>),
    /// Array pointer (possibly null).
    Arr(PointerValue<'ctx>),
    /// Vector pointer (possibly null).
    VecP(PointerValue<'ctx>),
    /// Function value (closure pointer, possibly null).
    Fun(PointerValue<'ctx>),
    /// RegExp pointer (possibly null).
    Reg(PointerValue<'ctx>),
    /// Date pointer (possibly null).
    Dat(PointerValue<'ctx>),
    /// Socket pointer (possibly null).
    Sock(PointerValue<'ctx>),
    /// Namespace value (interned, effectively never null).
    Ns(PointerValue<'ctx>),
    /// Pointer to an entry-block alloca holding a `{i32, i64}` box.
    Any(PointerValue<'ctx>),
    Void,
}

impl<'ctx> Cx<'ctx> {
    fn ptr(&self) -> inkwell::types::PointerType<'ctx> {
        self.context.ptr_type(AddressSpace::default())
    }

    fn basic_ty(&self, ty: Ty) -> BasicTypeEnum<'ctx> {
        match ty {
            Ty::Int | Ty::UInt => self.context.i32_type().into(),
            Ty::Number => self.context.f64_type().into(),
            Ty::Boolean => self.context.bool_type().into(),
            Ty::String => self.ptr().into(),
            Ty::Object(_)
            | Ty::Iface(_)
            | Ty::Array
            | Ty::Vector(_)
            | Ty::Function
            | Ty::RegExp
            | Ty::Date
            | Ty::Socket
            | Ty::Namespace => self.ptr().into(),
            Ty::Any => self.any_ty.into(),
            Ty::Void => unreachable!("void has no storage"),
        }
    }

    fn declare_function(&self, index: usize, f: &mir::Function) -> FunctionValue<'ctx> {
        let name = if index == 0 {
            // ABI with the runtime entry shim (runtime/src/ffi.rs).
            "vs_script".to_string()
        } else {
            format!("vs_fn{index}")
        };
        // Leading implicit params: environment (capturing functions),
        // then `this` (instance methods/constructors).
        let mut params: Vec<BasicMetadataTypeEnum> = Vec::new();
        if !f.captures.is_empty() {
            params.push(self.ptr().into());
        }
        if f.this_class.is_some() {
            params.push(self.ptr().into());
        }
        params.extend(
            f.locals[..f.param_count]
                .iter()
                .map(|&t| BasicMetadataTypeEnum::from(self.basic_ty(t))),
        );
        let fn_type = match f.ret {
            Ty::Void => self.context.void_type().fn_type(&params, false),
            t => self.basic_ty(t).fn_type(&params, false),
        };
        self.module.add_function(&name, fn_type, None)
    }

    /// Declares (once) and returns a runtime function.
    fn runtime_fn(
        &self,
        name: &str,
        ret: Option<BasicTypeEnum<'ctx>>,
        params: &[BasicMetadataTypeEnum<'ctx>],
    ) -> FunctionValue<'ctx> {
        if let Some(f) = self.module.get_function(name) {
            return f;
        }
        let ty = match ret {
            Some(r) => r.fn_type(params, false),
            None => self.context.void_type().fn_type(params, false),
        };
        self.module.add_function(name, ty, None)
    }
}

/// Loop/switch nesting for break/continue targets.
struct Frame<'ctx> {
    label: Option<String>,
    break_bb: BasicBlock<'ctx>,
    /// `None` for switch frames (continue skips them).
    continue_bb: Option<BasicBlock<'ctx>>,
    /// Exit-action stack depth at frame creation (cleanups above this run
    /// when jumping out to this frame).
    exit_depth: usize,
}

/// Per-function emission state.
struct FnCx<'a, 'ctx> {
    cx: &'a Cx<'ctx>,
    fns: &'a [FunctionValue<'ctx>],
    program: &'a mir::Program,
    function: FunctionValue<'ctx>,
    mir_fn: &'a mir::Function,
    locals: Vec<(PointerValue<'ctx>, Ty, bool)>,
    frames: Vec<Frame<'ctx>>,
    entry: BasicBlock<'ctx>,
    /// Environment parameter (array of cell pointers), when capturing.
    env_param: Option<PointerValue<'ctx>>,
    /// Cleanup obligations for early exits crossing `try` regions,
    /// innermost last.
    exit_actions: Vec<ExitAction<'a>>,
}

/// One pending cleanup on an early exit (return/break/continue) that
/// crosses a `try` region.
struct ExitAction<'a> {
    /// Pop the active setjmp handler (true while inside the try body).
    pop_handler: bool,
    /// Inline this `finally` block.
    finally: Option<&'a [Stmt]>,
}

impl<'a, 'ctx> FnCx<'a, 'ctx> {
    fn emit(
        cx: &'a Cx<'ctx>,
        fns: &'a [FunctionValue<'ctx>],
        program: &'a mir::Program,
        mir_fn: &'a mir::Function,
        function: FunctionValue<'ctx>,
    ) {
        // A function containing `try` calls _setjmp: after a longjmp,
        // registers roll back to their setjmp-time values, so locals
        // promoted out of allocas would silently lose writes (the C
        // "non-volatile locals are indeterminate" rule). Pin such
        // functions to optnone (+ the required noinline) — their locals
        // stay in memory, which the unwind scheme relies on. Everything
        // else gets the full pipeline.
        if contains_try(&mir_fn.body) {
            for name in ["optnone", "noinline"] {
                function.add_attribute(
                    inkwell::attributes::AttributeLoc::Function,
                    cx.context.create_enum_attribute(
                        inkwell::attributes::Attribute::get_named_enum_kind_id(name),
                        0,
                    ),
                );
            }
        }
        let entry = cx.context.append_basic_block(function, "entry");
        let body_bb = cx.context.append_basic_block(function, "body");
        cx.builder.position_at_end(body_bb);
        let mut fcx = FnCx {
            cx,
            fns,
            program,
            function,
            mir_fn,
            locals: Vec::new(),
            frames: Vec::new(),
            entry,
            env_param: (!mir_fn.captures.is_empty())
                .then(|| function.get_nth_param(0).expect("env").into_pointer_value()),
            exit_actions: Vec::new(),
        };
        // Local slots. Parameters copy their incoming values. Captured
        // locals are heap cells holding boxed values (closure conversion).
        let implicit =
            u32::from(!mir_fn.captures.is_empty()) + u32::from(mir_fn.this_class.is_some());
        for (i, &ty) in mir_fn.locals.iter().enumerate() {
            let is_cell = mir_fn.captured.get(i).copied().unwrap_or(false);
            if is_cell {
                let slot = fcx.entry_alloca(cx.ptr(), &format!("cell{i}"));
                fcx.locals.push((slot, ty, true));
                // Initial value: the incoming parameter or the type default.
                let init: Val = if i < mir_fn.param_count {
                    let arg = function.get_nth_param(i as u32 + implicit).expect("param");
                    fcx.wrap_basic(arg, ty)
                } else {
                    fcx.default_val(ty)
                };
                let boxed = match init {
                    Val::Any(p) => p,
                    other => fcx.box_value(other),
                };
                let newcell =
                    cx.runtime_fn("vs_cell_new", Some(cx.ptr().into()), &[cx.ptr().into()]);
                let cell = cx
                    .builder
                    .build_call(newcell, &[boxed.into()], "cell")
                    .expect("call")
                    .try_as_basic_value()
                    .basic()
                    .expect("value");
                cx.builder.build_store(slot, cell).expect("store");
                continue;
            }
            let slot = fcx.entry_alloca(cx.basic_ty(ty), &format!("local{i}"));
            fcx.locals.push((slot, ty, false));
            if i < mir_fn.param_count {
                let arg = function.get_nth_param(i as u32 + implicit).expect("param");
                cx.builder.build_store(slot, arg).expect("store");
            } else {
                // Non-param locals get their type's default (SPECS §3.11)
                // so reads-before-writes are defined.
                let init: BasicValueEnum = match ty {
                    Ty::Int | Ty::UInt => cx.context.i32_type().const_zero().into(),
                    Ty::Number => cx.context.f64_type().const_float(f64::NAN).into(),
                    Ty::Boolean => cx.context.bool_type().const_zero().into(),
                    Ty::String
                    | Ty::Object(_)
                    | Ty::Iface(_)
                    | Ty::Array
                    | Ty::Vector(_)
                    | Ty::Function
                    | Ty::RegExp
                    | Ty::Date
                    | Ty::Socket
                    | Ty::Namespace => cx.ptr().const_null().into(),
                    Ty::Any => cx.any_ty.const_zero().into(), // tag 0 = undefined
                    Ty::Void => unreachable!(),
                };
                cx.builder.build_store(slot, init).expect("store");
            }
        }
        // Script prologue: hand the Error descriptors to the runtime so
        // internal faults throw catchable objects (exc.rs contract), and
        // register ref/any statics as GC roots (gc.rs contract).
        if std::ptr::eq(mir_fn, &program.functions[0]) {
            if program.error_classes.len() == 6 {
                fcx.emit_error_registration();
            }
            fcx.emit_gc_root_registration();
        }
        // GC safepoint at function entry — only when this function can
        // create allocation debt itself: a body with direct allocation
        // sites, or captured locals (their heap cells allocate at entry).
        // Allocation-free functions skip it; any allocating callee keeps
        // its own entry safepoint, so collection delay stays bounded
        // (P22 safepoint elision; soundness note on expr_allocs).
        if stmts_alloc(&mir_fn.body) || mir_fn.captured.iter().any(|&c| c) {
            fcx.emit_safepoint();
        }
        for stmt in &mir_fn.body {
            fcx.stmt(stmt);
        }
        // Fall-off-the-end: void returns, value functions return their
        // type's default (sema proved this unreachable for non-void).
        if fcx.current_block_open() {
            fcx.emit_default_return();
        }
        // Entry block jumps to body once all allocas exist.
        cx.builder.position_at_end(entry);
        cx.builder.build_unconditional_branch(body_bb).expect("br");
    }

    fn current_block_open(&self) -> bool {
        self.cx
            .builder
            .get_insert_block()
            .is_some_and(|b| b.get_terminator().is_none())
    }

    fn emit_default_return(&mut self) {
        let b = &self.cx.builder;
        match self.mir_fn.ret {
            Ty::Void => b.build_return(None).expect("ret"),
            Ty::Int | Ty::UInt => b
                .build_return(Some(&self.cx.context.i32_type().const_zero()))
                .expect("ret"),
            Ty::Number => b
                .build_return(Some(&self.cx.context.f64_type().const_float(f64::NAN)))
                .expect("ret"),
            Ty::Boolean => b
                .build_return(Some(&self.cx.context.bool_type().const_zero()))
                .expect("ret"),
            Ty::String
            | Ty::Object(_)
            | Ty::Iface(_)
            | Ty::Array
            | Ty::Vector(_)
            | Ty::Function
            | Ty::RegExp
            | Ty::Date
            | Ty::Socket
            | Ty::Namespace => b
                .build_return(Some(&self.cx.ptr().const_null()))
                .expect("ret"),
            Ty::Any => b
                .build_return(Some(&self.cx.any_ty.const_zero()))
                .expect("ret"),
        };
    }

    fn entry_alloca(&self, ty: impl BasicType<'ctx>, name: &str) -> PointerValue<'ctx> {
        // All allocas live in the entry block so loops don't grow the stack.
        let current = self.cx.builder.get_insert_block().expect("block");
        match self.entry.get_terminator() {
            Some(t) => self.cx.builder.position_before(&t),
            None => self.cx.builder.position_at_end(self.entry),
        }
        let p = self.cx.builder.build_alloca(ty, name).expect("alloca");
        self.cx.builder.position_at_end(current);
        p
    }

    fn new_block(&self, name: &str) -> BasicBlock<'ctx> {
        self.cx.context.append_basic_block(self.function, name)
    }

    // --- statements --------------------------------------------------------

    fn stmt(&mut self, stmt: &'a Stmt) {
        if !self.current_block_open() {
            // Unreachable statement after break/continue/return.
            return;
        }
        match stmt {
            Stmt::Expr(e) => {
                self.expr(e);
            }
            Stmt::Assign(local, e) => {
                let v = self.expr(e);
                self.store_local(*local, v);
            }
            Stmt::Block(stmts) => {
                for s in stmts {
                    self.stmt(s);
                }
            }
            Stmt::If {
                cond,
                then_branch,
                else_branch,
            } => {
                let cv = self.expr(cond);
                let c = self.as_bool(cv);
                let then_bb = self.new_block("then");
                let end_bb = self.new_block("endif");
                let else_bb = match else_branch {
                    Some(_) => self.new_block("else"),
                    None => end_bb,
                };
                self.cx
                    .builder
                    .build_conditional_branch(c, then_bb, else_bb)
                    .expect("br");
                self.cx.builder.position_at_end(then_bb);
                self.stmt(then_branch);
                self.branch_if_open(end_bb);
                if let Some(e) = else_branch {
                    self.cx.builder.position_at_end(else_bb);
                    self.stmt(e);
                    self.branch_if_open(end_bb);
                }
                self.cx.builder.position_at_end(end_bb);
            }
            Stmt::While { label, cond, body } => {
                let cond_bb = self.new_block("while.cond");
                let body_bb = self.new_block("while.body");
                let end_bb = self.new_block("while.end");
                self.cx
                    .builder
                    .build_unconditional_branch(cond_bb)
                    .expect("br");
                self.cx.builder.position_at_end(cond_bb);
                let cv = self.expr(cond);
                let c = self.as_bool(cv);
                self.cx
                    .builder
                    .build_conditional_branch(c, body_bb, end_bb)
                    .expect("br");
                self.cx.builder.position_at_end(body_bb);
                if expr_allocs(cond) || stmt_allocs(body) {
                    self.emit_safepoint();
                }
                self.frames.push(Frame {
                    label: label.clone(),
                    break_bb: end_bb,
                    continue_bb: Some(cond_bb),
                    exit_depth: self.exit_actions.len(),
                });
                self.stmt(body);
                self.frames.pop();
                self.branch_if_open(cond_bb);
                self.cx.builder.position_at_end(end_bb);
            }
            Stmt::DoWhile { label, body, cond } => {
                let body_bb = self.new_block("do.body");
                let cond_bb = self.new_block("do.cond");
                let end_bb = self.new_block("do.end");
                self.cx
                    .builder
                    .build_unconditional_branch(body_bb)
                    .expect("br");
                self.cx.builder.position_at_end(body_bb);
                if expr_allocs(cond) || stmt_allocs(body) {
                    self.emit_safepoint();
                }
                self.frames.push(Frame {
                    label: label.clone(),
                    break_bb: end_bb,
                    continue_bb: Some(cond_bb),
                    exit_depth: self.exit_actions.len(),
                });
                self.stmt(body);
                self.frames.pop();
                self.branch_if_open(cond_bb);
                self.cx.builder.position_at_end(cond_bb);
                let cv = self.expr(cond);
                let c = self.as_bool(cv);
                self.cx
                    .builder
                    .build_conditional_branch(c, body_bb, end_bb)
                    .expect("br");
                self.cx.builder.position_at_end(end_bb);
            }
            Stmt::For {
                label,
                init,
                cond,
                update,
                body,
            } => {
                if let Some(init) = init {
                    self.stmt(init);
                }
                let cond_bb = self.new_block("for.cond");
                let body_bb = self.new_block("for.body");
                let update_bb = self.new_block("for.update");
                let end_bb = self.new_block("for.end");
                self.cx
                    .builder
                    .build_unconditional_branch(cond_bb)
                    .expect("br");
                self.cx.builder.position_at_end(cond_bb);
                match cond {
                    Some(c) => {
                        let cv = self.expr(c);
                        let c = self.as_bool(cv);
                        self.cx
                            .builder
                            .build_conditional_branch(c, body_bb, end_bb)
                            .expect("br");
                    }
                    None => {
                        self.cx
                            .builder
                            .build_unconditional_branch(body_bb)
                            .expect("br");
                    }
                }
                self.cx.builder.position_at_end(body_bb);
                if cond.as_ref().is_some_and(expr_allocs)
                    || update.as_ref().is_some_and(expr_allocs)
                    || stmt_allocs(body)
                {
                    self.emit_safepoint();
                }
                self.frames.push(Frame {
                    label: label.clone(),
                    break_bb: end_bb,
                    continue_bb: Some(update_bb),
                    exit_depth: self.exit_actions.len(),
                });
                self.stmt(body);
                self.frames.pop();
                self.branch_if_open(update_bb);
                self.cx.builder.position_at_end(update_bb);
                if let Some(u) = update {
                    self.expr(u);
                }
                self.branch_if_open(cond_bb);
                self.cx.builder.position_at_end(end_bb);
            }
            Stmt::Switch { scrutinee, cases } => self.switch(scrutinee, cases),
            Stmt::Break { label } => {
                let depth = self.frame_exit_depth(label.as_deref(), false);
                self.run_exit_actions(depth);
                let target = self.find_frame(label.as_deref(), false);
                self.cx
                    .builder
                    .build_unconditional_branch(target)
                    .expect("br");
            }
            Stmt::Continue { label } => {
                let depth = self.frame_exit_depth(label.as_deref(), true);
                self.run_exit_actions(depth);
                let target = self.find_frame(label.as_deref(), true);
                self.cx
                    .builder
                    .build_unconditional_branch(target)
                    .expect("br");
            }
            Stmt::Return { value } => {
                // Compute the value first, then unwind try cleanups.
                let precomputed = value.as_ref().map(|v| self.expr(v));
                self.run_exit_actions(0);
                match precomputed {
                    Some(v) => {
                        let basic = self.materialize(v);
                        self.cx.builder.build_return(Some(&basic)).expect("ret");
                    }
                    None => {
                        if self.mir_fn.ret == Ty::Void {
                            self.cx.builder.build_return(None).expect("ret");
                        } else {
                            // `return;` in a `*`-returning function.
                            self.emit_default_return();
                        }
                    }
                };
            }
            Stmt::Throw(value) => {
                let v = self.expr(value);
                let boxed = match v {
                    Val::Any(p) => p,
                    other => self.box_value(other),
                };
                let f = self
                    .cx
                    .runtime_fn("vs_throw", None, &[self.cx.ptr().into()]);
                self.cx
                    .builder
                    .build_call(f, &[boxed.into()], "")
                    .expect("call");
                self.cx.builder.build_unreachable().expect("unreachable");
                // Dead block for any following emission.
                let dead = self.new_block("after.throw");
                self.cx.builder.position_at_end(dead);
            }
            Stmt::Try {
                body,
                catches,
                finally,
            } => self.emit_try(body, catches, finally.as_deref()),
            Stmt::Empty => {}
        }
    }

    /// `try/catch/finally` on the setjmp scheme (SPECS §7, documented v1
    /// choice). The buffer lives in this frame; `vs_throw` longjmps back
    /// here with the boxed exception.
    fn emit_try(
        &mut self,
        body: &'a [Stmt],
        catches: &'a [mir::Catch],
        finally: Option<&'a [Stmt]>,
    ) {
        let cx = self.cx;
        // Generous jmp_buf storage (macOS arm64 needs 192 bytes).
        let buf_ty = cx.context.i8_type().array_type(512);
        let buf = self.entry_alloca(buf_ty, "jmpbuf");
        // On Windows the runtime ships its own non-unwinding pair
        // (winjmp.rs) — msvcrt longjmp would SEH-unwind through frames
        // that carry no unwind tables.
        let setjmp = cx.runtime_fn(
            if cx.windows { "vs_setjmp" } else { "_setjmp" },
            Some(cx.context.i32_type().into()),
            &[cx.ptr().into()],
        );
        let call = self
            .cx
            .builder
            .build_call(setjmp, &[buf.into()], "sj")
            .expect("call");
        // returns_twice: locals must not be cached across this call.
        let rt = cx.context.create_enum_attribute(
            inkwell::attributes::Attribute::get_named_enum_kind_id("returns_twice"),
            0,
        );
        call.add_attribute(inkwell::attributes::AttributeLoc::Function, rt);
        let r = call
            .try_as_basic_value()
            .basic()
            .expect("value")
            .into_int_value();
        let body_bb = self.new_block("try.body");
        let dispatch_bb = self.new_block("try.dispatch");
        let end_bb = self.new_block("try.end");
        let is_zero = self
            .cx
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                r,
                cx.context.i32_type().const_zero(),
                "first",
            )
            .expect("cmp");
        self.cx
            .builder
            .build_conditional_branch(is_zero, body_bb, dispatch_bb)
            .expect("br");

        // Body: handler active; early exits must pop it and run finally.
        self.cx.builder.position_at_end(body_bb);
        let push = cx.runtime_fn("vs_push_handler", None, &[cx.ptr().into()]);
        self.cx
            .builder
            .build_call(push, &[buf.into()], "")
            .expect("call");
        self.exit_actions.push(ExitAction {
            pop_handler: true,
            finally,
        });
        for st in body {
            self.stmt(st);
        }
        self.exit_actions.pop();
        if self.current_block_open() {
            let pop = cx.runtime_fn("vs_pop_handler", None, &[]);
            self.cx.builder.build_call(pop, &[], "").expect("call");
            if let Some(f) = finally {
                for st in f {
                    self.stmt(st);
                }
            }
            self.branch_if_open(end_bb);
        }

        // Dispatch: match catches in order by `is` against the binding type.
        self.cx.builder.position_at_end(dispatch_bb);
        let exc = self.entry_alloca(cx.any_ty, "exc");
        let take = cx.runtime_fn("vs_take_exception", None, &[cx.ptr().into()]);
        self.cx
            .builder
            .build_call(take, &[exc.into()], "")
            .expect("call");
        let mut matched_all = false;
        for c in catches {
            let (_, binding_ty, _) = self.locals[c.binding.0 as usize];
            let is_catch_all = matches!(binding_ty, Ty::Any);
            let body_bb = self.new_block("catch.body");
            let next_bb = self.new_block("catch.next");
            if is_catch_all {
                self.cx
                    .builder
                    .build_unconditional_branch(body_bb)
                    .expect("br");
                matched_all = true;
            } else {
                let cond = self.exc_matches(exc, binding_ty);
                self.cx
                    .builder
                    .build_conditional_branch(cond, body_bb, next_bb)
                    .expect("br");
            }
            self.cx.builder.position_at_end(body_bb);
            let bound = self.unbox_any_ptr(exc, binding_ty);
            self.store_local(c.binding, bound);
            self.exit_actions.push(ExitAction {
                pop_handler: false,
                finally,
            });
            for st in &c.body {
                self.stmt(st);
            }
            self.exit_actions.pop();
            if self.current_block_open() {
                if let Some(f) = finally {
                    for st in f {
                        self.stmt(st);
                    }
                }
                self.branch_if_open(end_bb);
            }
            self.cx.builder.position_at_end(next_bb);
            if matched_all {
                break;
            }
        }
        if !matched_all {
            // No catch matched: run finally, rethrow.
            if let Some(f) = finally {
                for st in f {
                    self.stmt(st);
                }
            }
            if self.current_block_open() {
                let rethrow = cx.runtime_fn("vs_throw", None, &[cx.ptr().into()]);
                self.cx
                    .builder
                    .build_call(rethrow, &[exc.into()], "")
                    .expect("call");
                self.cx.builder.build_unreachable().expect("unreachable");
            }
        } else if self.current_block_open() {
            // Unreachable trailing next-block after a catch-all.
            self.cx.builder.build_unreachable().expect("unreachable");
        }
        self.cx.builder.position_at_end(end_bb);
    }

    /// `exc is T` for catch dispatch.
    fn exc_matches(&mut self, exc: PointerValue<'ctx>, ty: Ty) -> IntValue<'ctx> {
        let cx = self.cx;
        let i32t = cx.context.i32_type();
        let call = match ty {
            Ty::Object(class) => {
                let f = cx.runtime_fn(
                    "vs_any_is_class",
                    Some(i32t.into()),
                    &[cx.ptr().into(), cx.ptr().into()],
                );
                let rtti = cx.classes[class as usize].rtti;
                self.cx
                    .builder
                    .build_call(f, &[exc.into(), rtti.into()], "m")
            }
            Ty::Iface(iface) => {
                let f = cx.runtime_fn(
                    "vs_any_is_iface",
                    Some(i32t.into()),
                    &[cx.ptr().into(), i32t.into()],
                );
                self.cx.builder.build_call(
                    f,
                    &[exc.into(), i32t.const_int(u64::from(iface), false).into()],
                    "m",
                )
            }
            Ty::Array => {
                let f = cx.runtime_fn("vs_any_is_array", Some(i32t.into()), &[cx.ptr().into()]);
                self.cx.builder.build_call(f, &[exc.into()], "m")
            }
            Ty::Vector(inst) => {
                let f = cx.runtime_fn(
                    "vs_any_is_vector",
                    Some(i32t.into()),
                    &[cx.ptr().into(), i32t.into()],
                );
                self.cx.builder.build_call(
                    f,
                    &[exc.into(), i32t.const_int(u64::from(inst), false).into()],
                    "m",
                )
            }
            other => {
                let f = cx.runtime_fn(
                    "vs_any_is",
                    Some(i32t.into()),
                    &[cx.ptr().into(), i32t.into()],
                );
                let t = i32t.const_int(u64::from(ty_tag(other)), false);
                self.cx.builder.build_call(f, &[exc.into(), t.into()], "m")
            }
        }
        .expect("call");
        self.nonzero(
            call.try_as_basic_value()
                .basic()
                .expect("value")
                .into_int_value(),
        )
    }

    /// Registers the prelude Error descriptors with the runtime.
    /// Interns a namespace literal by URI (runtime keeps identity).
    fn intern_namespace(&mut self, uri: &str) -> Val<'ctx> {
        let cx = self.cx;
        let (g, len) = self.utf8_lit(uri, "nsuri");
        let f = cx.runtime_fn(
            "vs_namespace_intern",
            Some(cx.ptr().into()),
            &[cx.ptr().into(), cx.context.i32_type().into()],
        );
        let call = self
            .cx
            .builder
            .build_call(f, &[g.into(), len.into()], "ns")
            .expect("call");
        Val::Ns(
            call.try_as_basic_value()
                .basic()
                .expect("value")
                .into_pointer_value(),
        )
    }

    /// A UTF-8 string literal global + its length constant.
    fn utf8_lit(&mut self, text: &str, name: &str) -> (PointerValue<'ctx>, IntValue<'ctx>) {
        let g = self
            .cx
            .builder
            .build_global_string_ptr(text, name)
            .expect("global");
        (
            g.as_pointer_value(),
            self.cx
                .context
                .i32_type()
                .const_int(text.len() as u64, false),
        )
    }

    /// GC safepoint call (gc.rs: collection happens only here). The
    /// declaration carries `memory(inaccessiblemem: readwrite)` +
    /// `nounwind`: the collector only frees memory the program can no
    /// longer reach and mutates only its own bookkeeping, so from the
    /// optimizer's model the call touches no program-visible state — LLVM
    /// may keep values in registers across it (the conservative scan sees
    /// registers and stack either way).
    fn emit_safepoint(&mut self) {
        let cx = self.cx;
        let f = cx.runtime_fn("vs_gc_safepoint", None, &[]);
        // MemoryEffects encoding (LLVM): 2 bits per location, locations
        // argmem=0 / inaccessiblemem=1 / other=2; ModRef=3.
        // inaccessiblemem: readwrite, everything else: none -> 3 << 2.
        let memory_kind = inkwell::attributes::Attribute::get_named_enum_kind_id("memory");
        let nounwind_kind = inkwell::attributes::Attribute::get_named_enum_kind_id("nounwind");
        f.add_attribute(
            inkwell::attributes::AttributeLoc::Function,
            cx.context.create_enum_attribute(memory_kind, 3 << 2),
        );
        f.add_attribute(
            inkwell::attributes::AttributeLoc::Function,
            cx.context.create_enum_attribute(nounwind_kind, 0),
        );
        self.cx.builder.build_call(f, &[], "sp").expect("call");
    }

    /// Registers every ref- or any-typed static field as a GC root
    /// (numeric statics can't hold references; skipping them avoids
    /// f64 bit patterns pinning random blocks).
    fn emit_gc_root_registration(&mut self) {
        let cx = self.cx;
        let i32t = cx.context.i32_type();
        let f = cx.runtime_fn("vs_gc_add_root", None, &[cx.ptr().into(), i32t.into()]);
        for (ci, class) in self.program.classes.iter().enumerate() {
            for (si, &ty) in class.statics.iter().enumerate() {
                let words: u64 = match ty {
                    Ty::Any => 2,
                    Ty::String
                    | Ty::Object(_)
                    | Ty::Iface(_)
                    | Ty::Array
                    | Ty::Vector(_)
                    | Ty::Function
                    | Ty::RegExp
                    | Ty::Date
                    | Ty::Socket
                    | Ty::Namespace => 1,
                    Ty::Int | Ty::UInt | Ty::Number | Ty::Boolean | Ty::Void => continue,
                };
                let g = cx.classes[ci].statics[si];
                self.cx
                    .builder
                    .build_call(f, &[g.into(), i32t.const_int(words, false).into()], "")
                    .expect("call");
            }
        }
    }

    fn emit_error_registration(&mut self) {
        let cx = self.cx;
        let n = self.program.error_classes.len() as u32;
        let descs = self.entry_alloca(cx.ptr().array_type(n), "errdescs");
        let sizes = self.entry_alloca(cx.context.i64_type().array_type(n), "errsizes");
        for (i, &class) in self.program.error_classes.iter().enumerate() {
            let art = &cx.classes[class as usize];
            let (rtti, size) = (art.rtti, art.size);
            let dslot = unsafe {
                self.cx.builder.build_in_bounds_gep(
                    cx.ptr().array_type(n),
                    descs,
                    &[
                        cx.context.i32_type().const_zero(),
                        cx.context.i32_type().const_int(i as u64, false),
                    ],
                    "d",
                )
            }
            .expect("gep");
            self.cx.builder.build_store(dslot, rtti).expect("store");
            let sslot = unsafe {
                self.cx.builder.build_in_bounds_gep(
                    cx.context.i64_type().array_type(n),
                    sizes,
                    &[
                        cx.context.i32_type().const_zero(),
                        cx.context.i32_type().const_int(i as u64, false),
                    ],
                    "s",
                )
            }
            .expect("gep");
            self.cx
                .builder
                .build_store(sslot, cx.context.i64_type().const_int(size, false))
                .expect("store");
        }
        let f = cx.runtime_fn(
            "vs_register_errors",
            None,
            &[
                cx.context.i32_type().into(),
                cx.ptr().into(),
                cx.ptr().into(),
            ],
        );
        self.cx
            .builder
            .build_call(
                f,
                &[
                    cx.context.i32_type().const_int(u64::from(n), false).into(),
                    descs.into(),
                    sizes.into(),
                ],
                "",
            )
            .expect("call");
    }

    fn branch_if_open(&self, target: BasicBlock<'ctx>) {
        if self.current_block_open() {
            self.cx
                .builder
                .build_unconditional_branch(target)
                .expect("br");
        }
    }

    /// Type default constant (SPECS §3.11) as a value.
    fn default_val(&mut self, ty: Ty) -> Val<'ctx> {
        let cx = self.cx;
        match ty {
            Ty::Int => Val::Int(cx.context.i32_type().const_zero()),
            Ty::UInt => Val::UInt(cx.context.i32_type().const_zero()),
            Ty::Number => Val::Num(cx.context.f64_type().const_float(f64::NAN)),
            Ty::Boolean => Val::Bool(cx.context.bool_type().const_zero()),
            Ty::String => Val::Str(cx.ptr().const_null()),
            Ty::Object(_) | Ty::Iface(_) => Val::Obj(cx.ptr().const_null()),
            Ty::Array => Val::Arr(cx.ptr().const_null()),
            Ty::Vector(_) => Val::VecP(cx.ptr().const_null()),
            Ty::Function => Val::Fun(cx.ptr().const_null()),
            Ty::RegExp => Val::Reg(cx.ptr().const_null()),
            Ty::Date => Val::Dat(cx.ptr().const_null()),
            Ty::Socket => Val::Sock(cx.ptr().const_null()),
            Ty::Namespace => Val::Ns(cx.ptr().const_null()),
            Ty::Any => {
                let slot = self.entry_alloca(cx.any_ty, "undef");
                self.cx
                    .builder
                    .build_store(slot, cx.any_ty.const_zero())
                    .expect("store");
                Val::Any(slot)
            }
            Ty::Void => Val::Void,
        }
    }

    /// Runs cleanup obligations above `to_depth` (early exits leaving `try`
    /// regions): pop live handlers and inline pending `finally` blocks.
    fn run_exit_actions(&mut self, to_depth: usize) {
        // Take ownership to avoid double-runs while emitting finallys.
        let actions: Vec<ExitAction> = self.exit_actions.split_off(to_depth);
        for action in actions.iter().rev() {
            if action.pop_handler {
                let f = self.cx.runtime_fn("vs_pop_handler", None, &[]);
                self.cx.builder.build_call(f, &[], "").expect("call");
            }
            if let Some(finally) = action.finally {
                for st in finally {
                    self.stmt(st);
                }
            }
        }
        // Restore the stack (the emitting scope still owns them).
        self.exit_actions.extend(actions);
    }

    /// Exit-action depth of the jump target frame (see `find_frame`).
    fn frame_exit_depth(&self, label: Option<&str>, for_continue: bool) -> usize {
        for frame in self.frames.iter().rev() {
            let label_matches = match label {
                Some(l) => frame.label.as_deref() == Some(l),
                None => true,
            };
            if !label_matches {
                continue;
            }
            if for_continue && frame.continue_bb.is_none() && label.is_none() {
                continue;
            }
            return frame.exit_depth;
        }
        0
    }

    fn find_frame(&self, label: Option<&str>, for_continue: bool) -> BasicBlock<'ctx> {
        for frame in self.frames.iter().rev() {
            let label_matches = match label {
                Some(l) => frame.label.as_deref() == Some(l),
                None => true,
            };
            if !label_matches {
                continue;
            }
            if for_continue {
                if let Some(c) = frame.continue_bb {
                    return c;
                }
                // Unlabeled continue inside a switch: keep looking outward.
                if label.is_none() {
                    continue;
                }
            } else {
                return frame.break_bb;
            }
        }
        unreachable!("sema validated jump targets")
    }

    /// `switch` (§12.11 with AS3 strict-equality matching): tests run in
    /// source order (skipping `default`), bodies fall through in source
    /// order including `default`.
    fn switch(&mut self, scrutinee: &'a mir::Expr, cases: &'a [mir::Case]) {
        let scrut = self.expr(scrutinee);
        let end_bb = self.new_block("switch.end");
        let body_bbs: Vec<BasicBlock> = (0..cases.len())
            .map(|i| self.new_block(&format!("case{i}.body")))
            .collect();
        let default_target = cases
            .iter()
            .position(|c| c.test.is_none())
            .map(|i| body_bbs[i])
            .unwrap_or(end_bb);

        // Test chain.
        for (i, case) in cases.iter().enumerate() {
            let Some(test) = &case.test else { continue };
            let next_test = self.new_block(&format!("case{}.test", i + 1));
            let t = self.expr(test);
            let eq = self.strict_equals(scrut, t);
            self.cx
                .builder
                .build_conditional_branch(eq, body_bbs[i], next_test)
                .expect("br");
            self.cx.builder.position_at_end(next_test);
        }
        self.cx
            .builder
            .build_unconditional_branch(default_target)
            .expect("br");

        // Bodies with fall-through.
        self.frames.push(Frame {
            label: None,
            break_bb: end_bb,
            continue_bb: None,
            exit_depth: self.exit_actions.len(),
        });
        for (i, case) in cases.iter().enumerate() {
            self.cx.builder.position_at_end(body_bbs[i]);
            for s in &case.body {
                self.stmt(s);
            }
            let next = body_bbs.get(i + 1).copied().unwrap_or(end_bb);
            self.branch_if_open(next);
        }
        self.frames.pop();
        self.cx.builder.position_at_end(end_bb);
    }

    // --- expressions ---------------------------------------------------------

    fn expr(&mut self, e: &mir::Expr) -> Val<'ctx> {
        let cx = self.cx;
        match &e.kind {
            ExprKind::Int(v) => Val::Int(cx.context.i32_type().const_int(*v as u32 as u64, false)),
            ExprKind::UInt(v) => Val::UInt(cx.context.i32_type().const_int(u64::from(*v), false)),
            ExprKind::Number(v) => Val::Num(cx.context.f64_type().const_float(*v)),
            ExprKind::Bool(v) => Val::Bool(cx.context.bool_type().const_int(u64::from(*v), false)),
            ExprKind::RegExpLit(pat, flags) => {
                let pat_g = self
                    .cx
                    .builder
                    .build_global_string_ptr(pat, "relit")
                    .expect("global");
                let fl_g = self
                    .cx
                    .builder
                    .build_global_string_ptr(flags, "reflags")
                    .expect("global");
                let i32t = cx.context.i32_type();
                let f = cx.runtime_fn(
                    "vs_regexp_lit",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into(), i32t.into(), cx.ptr().into(), i32t.into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(
                        f,
                        &[
                            pat_g.as_pointer_value().into(),
                            i32t.const_int(pat.len() as u64, false).into(),
                            fl_g.as_pointer_value().into(),
                            i32t.const_int(flags.len() as u64, false).into(),
                        ],
                        "re",
                    )
                    .expect("call");
                Val::Reg(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            ExprKind::NewRegExp(args) => {
                let pat = self.str_arg(&args[0]);
                let flags = match args.get(1) {
                    Some(a) => self.str_arg(a),
                    None => cx.ptr().const_null(),
                };
                let f = cx.runtime_fn(
                    "vs_regexp_new",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into(), cx.ptr().into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[pat.into(), flags.into()], "re")
                    .expect("call");
                Val::Reg(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            ExprKind::CallRegex(op, operands) => self.call_regex(*op, operands),
            ExprKind::NewDate(args) => {
                let arr = self.stage_f64_args(args, "dateparts");
                let i32t = cx.context.i32_type();
                let f = cx.runtime_fn(
                    "vs_date_new",
                    Some(cx.ptr().into()),
                    &[i32t.into(), cx.ptr().into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(
                        f,
                        &[i32t.const_int(args.len() as u64, false).into(), arr.into()],
                        "date",
                    )
                    .expect("call");
                Val::Dat(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            ExprKind::CallDate(f, operands) => self.call_date(*f, operands),
            ExprKind::CallSocket(op, operands) => self.call_socket(*op, operands),
            ExprKind::NamespaceVal(id) => {
                let uri = &self.program.namespace_uris[*id as usize].clone();
                self.intern_namespace(uri)
            }
            ExprKind::NewNamespace(uri) => {
                let u = self.str_arg(uri);
                let f = cx.runtime_fn(
                    "vs_namespace_new",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[u.into()], "ns")
                    .expect("call");
                Val::Ns(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            ExprKind::NsUri(q) => {
                let p = match self.expr(q) {
                    Val::Ns(p) => p,
                    _ => unreachable!("NsUri operand"),
                };
                let f = cx.runtime_fn(
                    "vs_namespace_uri",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[p.into()], "uri")
                    .expect("call");
                Val::Str(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            ExprKind::NsGet(recv, q, name) => {
                let recv_p = self.as_any_ptr(recv);
                let q = match self.expr(q) {
                    Val::Ns(p) => p,
                    _ => unreachable!("qualifier"),
                };
                let (name_g, name_len) = self.utf8_lit(name, "nsn");
                let out = self.entry_alloca(cx.any_ty, "nsget");
                let i32t = cx.context.i32_type();
                let f = cx.runtime_fn(
                    "vs_ns_get",
                    None,
                    &[
                        cx.ptr().into(),
                        cx.ptr().into(),
                        cx.ptr().into(),
                        i32t.into(),
                        cx.ptr().into(),
                    ],
                );
                self.cx
                    .builder
                    .build_call(
                        f,
                        &[
                            recv_p.into(),
                            q.into(),
                            name_g.into(),
                            name_len.into(),
                            out.into(),
                        ],
                        "",
                    )
                    .expect("call");
                Val::Any(out)
            }
            ExprKind::NsCall(recv, q, name, args) => {
                let recv_p = self.as_any_ptr(recv);
                let q = match self.expr(q) {
                    Val::Ns(p) => p,
                    _ => unreachable!("qualifier"),
                };
                let (name_g, name_len) = self.utf8_lit(name, "nsn");
                let i32t = cx.context.i32_type();
                let argc = args.len() as u32;
                let arr_ty = cx.any_ty.array_type(argc.max(1));
                let arr = self.entry_alloca(arr_ty, "nsargs");
                for (i, a) in args.iter().enumerate() {
                    let p = self.as_any_ptr(a);
                    let v = self
                        .cx
                        .builder
                        .build_load(cx.any_ty, p, "arg")
                        .expect("load");
                    let slot = unsafe {
                        self.cx.builder.build_in_bounds_gep(
                            arr_ty,
                            arr,
                            &[i32t.const_zero(), i32t.const_int(i as u64, false)],
                            "slot",
                        )
                    }
                    .expect("gep");
                    self.cx.builder.build_store(slot, v).expect("store");
                }
                let out = self.entry_alloca(cx.any_ty, "nscall");
                let f = cx.runtime_fn(
                    "vs_ns_call",
                    None,
                    &[
                        cx.ptr().into(),
                        cx.ptr().into(),
                        cx.ptr().into(),
                        i32t.into(),
                        i32t.into(),
                        cx.ptr().into(),
                        cx.ptr().into(),
                    ],
                );
                self.cx
                    .builder
                    .build_call(
                        f,
                        &[
                            recv_p.into(),
                            q.into(),
                            name_g.into(),
                            name_len.into(),
                            i32t.const_int(u64::from(argc), false).into(),
                            arr.into(),
                            out.into(),
                        ],
                        "",
                    )
                    .expect("call");
                Val::Any(out)
            }
            ExprKind::Str(s) => {
                let global = self
                    .cx
                    .builder
                    .build_global_string_ptr(s, "strlit")
                    .expect("global");
                let len = cx.context.i32_type().const_int(s.len() as u64, false);
                let f = cx.runtime_fn(
                    "vs_string_from_utf8",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into(), cx.context.i32_type().into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[global.as_pointer_value().into(), len.into()], "str")
                    .expect("call");
                Val::Str(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            ExprKind::Null => Val::Str(cx.ptr().const_null()),
            ExprKind::Undefined => {
                let slot = self.entry_alloca(cx.any_ty, "undef");
                self.cx
                    .builder
                    .build_store(slot, cx.any_ty.const_zero())
                    .expect("store");
                Val::Any(slot)
            }
            ExprKind::LocalGet(id) => self.load_local(*id),
            ExprKind::LocalSet(id, v) => {
                let v = self.expr(v);
                self.store_local(*id, v);
                // Reads back the slot so aliasing writes don't leak through.
                self.load_local(*id)
            }
            ExprKind::CallFn(id, args) => {
                let mut argv: Vec<BasicMetadataValueEnum> = Vec::new();
                for a in args {
                    let v = self.expr(a);
                    argv.push(self.materialize(v).into());
                }
                let call = self
                    .cx
                    .builder
                    .build_call(self.fns[id.0 as usize], &argv, "call")
                    .expect("call");
                match e.ty {
                    Ty::Void => Val::Void,
                    ty => self.wrap_basic(call.try_as_basic_value().basic().expect("value"), ty),
                }
            }
            ExprKind::CallBuiltin(b, args) => self.builtin(*b, args),
            ExprKind::CallStrMethod(m, recv, args) => self.str_method(*m, recv, args),
            ExprKind::CallNumMethod(m, recv, args) => self.num_method(*m, recv, args),
            ExprKind::StrLen(recv) => {
                let r = self.expr(recv);
                let Val::Str(p) = r else {
                    unreachable!("StrLen receiver")
                };
                let f = cx.runtime_fn(
                    "vs_string_length",
                    Some(cx.context.i32_type().into()),
                    &[cx.ptr().into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[p.into()], "len")
                    .expect("call");
                Val::Int(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_int_value(),
                )
            }
            ExprKind::Unary(op, v) => self.unary(*op, v),
            ExprKind::IncDec {
                target,
                is_inc,
                is_prefix,
            } => self.inc_dec(*target, *is_inc, *is_prefix),
            ExprKind::Binary(op, l, r) => self.binary(*op, l, r),
            ExprKind::Logical { is_and, lhs, rhs } => self.logical(*is_and, lhs, rhs, e.ty),
            ExprKind::Conditional(c, t, f) => self.conditional(c, t, f, e.ty),
            ExprKind::Is(v, target) => {
                if *target == Ty::Array {
                    let p = self.as_any_ptr(v);
                    let f = cx.runtime_fn(
                        "vs_any_is_array",
                        Some(cx.context.i32_type().into()),
                        &[cx.ptr().into()],
                    );
                    let call = self
                        .cx
                        .builder
                        .build_call(f, &[p.into()], "is_a")
                        .expect("call");
                    return Val::Bool(
                        self.nonzero(
                            call.try_as_basic_value()
                                .basic()
                                .expect("value")
                                .into_int_value(),
                        ),
                    );
                }
                if let Ty::Vector(inst) = *target {
                    let p = self.as_any_ptr(v);
                    let f = cx.runtime_fn(
                        "vs_any_is_vector",
                        Some(cx.context.i32_type().into()),
                        &[cx.ptr().into(), cx.context.i32_type().into()],
                    );
                    let id = cx.context.i32_type().const_int(u64::from(inst), false);
                    let call = self
                        .cx
                        .builder
                        .build_call(f, &[p.into(), id.into()], "is_v")
                        .expect("call");
                    return Val::Bool(
                        self.nonzero(
                            call.try_as_basic_value()
                                .basic()
                                .expect("value")
                                .into_int_value(),
                        ),
                    );
                }
                if let Ty::Object(class) = *target {
                    let p = self.as_any_ptr(v);
                    let f = cx.runtime_fn(
                        "vs_any_is_class",
                        Some(cx.context.i32_type().into()),
                        &[cx.ptr().into(), cx.ptr().into()],
                    );
                    let rtti = cx.classes[class as usize].rtti;
                    let call = self
                        .cx
                        .builder
                        .build_call(f, &[p.into(), rtti.into()], "is_c")
                        .expect("call");
                    return Val::Bool(
                        self.nonzero(
                            call.try_as_basic_value()
                                .basic()
                                .expect("value")
                                .into_int_value(),
                        ),
                    );
                }
                if let Ty::Iface(iface) = *target {
                    let p = self.as_any_ptr(v);
                    let f = cx.runtime_fn(
                        "vs_any_is_iface",
                        Some(cx.context.i32_type().into()),
                        &[cx.ptr().into(), cx.context.i32_type().into()],
                    );
                    let id = cx.context.i32_type().const_int(u64::from(iface), false);
                    let call = self
                        .cx
                        .builder
                        .build_call(f, &[p.into(), id.into()], "is_i")
                        .expect("call");
                    return Val::Bool(
                        self.nonzero(
                            call.try_as_basic_value()
                                .basic()
                                .expect("value")
                                .into_int_value(),
                        ),
                    );
                }
                let p = self.as_any_ptr(v);
                let f = cx.runtime_fn(
                    "vs_any_is",
                    Some(cx.context.i32_type().into()),
                    &[cx.ptr().into(), cx.context.i32_type().into()],
                );
                let t = cx
                    .context
                    .i32_type()
                    .const_int(u64::from(ty_tag(*target)), false);
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[p.into(), t.into()], "is")
                    .expect("call");
                Val::Bool(
                    self.nonzero(
                        call.try_as_basic_value()
                            .basic()
                            .expect("value")
                            .into_int_value(),
                    ),
                )
            }
            ExprKind::As(v, target) => {
                if *target == Ty::Array {
                    let p = self.as_any_ptr(v);
                    let f =
                        cx.runtime_fn("vs_any_as_array", Some(cx.ptr().into()), &[cx.ptr().into()]);
                    let call = self
                        .cx
                        .builder
                        .build_call(f, &[p.into()], "as_a")
                        .expect("call");
                    return Val::Arr(
                        call.try_as_basic_value()
                            .basic()
                            .expect("value")
                            .into_pointer_value(),
                    );
                }
                if let Ty::Vector(inst) = *target {
                    let p = self.as_any_ptr(v);
                    let f = cx.runtime_fn(
                        "vs_any_as_vector",
                        Some(cx.ptr().into()),
                        &[cx.ptr().into(), cx.context.i32_type().into()],
                    );
                    let id = cx.context.i32_type().const_int(u64::from(inst), false);
                    let call = self
                        .cx
                        .builder
                        .build_call(f, &[p.into(), id.into()], "as_v")
                        .expect("call");
                    return Val::VecP(
                        call.try_as_basic_value()
                            .basic()
                            .expect("value")
                            .into_pointer_value(),
                    );
                }
                if let Ty::Object(class) = *target {
                    let p = self.as_any_ptr(v);
                    let f = cx.runtime_fn(
                        "vs_any_as_class",
                        Some(cx.ptr().into()),
                        &[cx.ptr().into(), cx.ptr().into()],
                    );
                    let rtti = cx.classes[class as usize].rtti;
                    let call = self
                        .cx
                        .builder
                        .build_call(f, &[p.into(), rtti.into()], "as_c")
                        .expect("call");
                    return Val::Obj(
                        call.try_as_basic_value()
                            .basic()
                            .expect("value")
                            .into_pointer_value(),
                    );
                }
                if *target == Ty::Date {
                    let p = self.as_any_ptr(v);
                    let f =
                        cx.runtime_fn("vs_any_as_date", Some(cx.ptr().into()), &[cx.ptr().into()]);
                    let call = self
                        .cx
                        .builder
                        .build_call(f, &[p.into()], "as_d")
                        .expect("call");
                    return Val::Dat(
                        call.try_as_basic_value()
                            .basic()
                            .expect("value")
                            .into_pointer_value(),
                    );
                }
                if *target == Ty::RegExp {
                    let p = self.as_any_ptr(v);
                    let f = cx.runtime_fn(
                        "vs_any_as_regexp",
                        Some(cx.ptr().into()),
                        &[cx.ptr().into()],
                    );
                    let call = self
                        .cx
                        .builder
                        .build_call(f, &[p.into()], "as_re")
                        .expect("call");
                    return Val::Reg(
                        call.try_as_basic_value()
                            .basic()
                            .expect("value")
                            .into_pointer_value(),
                    );
                }
                if let Ty::Iface(iface) = *target {
                    let p = self.as_any_ptr(v);
                    let f = cx.runtime_fn(
                        "vs_any_as_iface",
                        Some(cx.ptr().into()),
                        &[cx.ptr().into(), cx.context.i32_type().into()],
                    );
                    let id = cx.context.i32_type().const_int(u64::from(iface), false);
                    let call = self
                        .cx
                        .builder
                        .build_call(f, &[p.into(), id.into()], "as_i")
                        .expect("call");
                    return Val::Obj(
                        call.try_as_basic_value()
                            .basic()
                            .expect("value")
                            .into_pointer_value(),
                    );
                }
                let p = self.as_any_ptr(v);
                if *target == Ty::String {
                    let f = cx.runtime_fn(
                        "vs_any_as_string",
                        Some(cx.ptr().into()),
                        &[cx.ptr().into()],
                    );
                    let call = self
                        .cx
                        .builder
                        .build_call(f, &[p.into()], "as_str")
                        .expect("call");
                    Val::Str(
                        call.try_as_basic_value()
                            .basic()
                            .expect("value")
                            .into_pointer_value(),
                    )
                } else {
                    let out = self.entry_alloca(cx.any_ty, "as_out");
                    let f = cx.runtime_fn(
                        "vs_any_as",
                        None,
                        &[
                            cx.ptr().into(),
                            cx.context.i32_type().into(),
                            cx.ptr().into(),
                        ],
                    );
                    let t = cx
                        .context
                        .i32_type()
                        .const_int(u64::from(ty_tag(*target)), false);
                    self.cx
                        .builder
                        .build_call(f, &[p.into(), t.into(), out.into()], "")
                        .expect("call");
                    Val::Any(out)
                }
            }
            ExprKind::Conv(conv, v) => self.convert(*conv, v),
            ExprKind::Comma(l, r) => {
                self.expr(l);
                self.expr(r)
            }
            ExprKind::ObjectLit(props) => {
                // A fresh dynamic Object instance (class 0), props via the
                // runtime expando path.
                let art = &cx.classes[0];
                let (rtti, size) = (art.rtti, art.size);
                let alloc = cx.runtime_fn(
                    "vs_alloc_object",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into(), cx.context.i64_type().into()],
                );
                let obj = self
                    .cx
                    .builder
                    .build_call(
                        alloc,
                        &[
                            rtti.into(),
                            cx.context.i64_type().const_int(size, false).into(),
                        ],
                        "objlit",
                    )
                    .expect("call")
                    .try_as_basic_value()
                    .basic()
                    .expect("value")
                    .into_pointer_value();
                let boxed = self.box_value(Val::Obj(obj));
                let setp = cx.runtime_fn(
                    "vs_any_set_prop",
                    None,
                    &[cx.ptr().into(), cx.ptr().into(), cx.ptr().into()],
                );
                for (name, value) in props {
                    let key = self.const_string(name);
                    let v = self.expr(value);
                    let vboxed = match v {
                        Val::Any(p) => p,
                        other => self.box_value(other),
                    };
                    self.cx
                        .builder
                        .build_call(setp, &[boxed.into(), key.into(), vboxed.into()], "")
                        .expect("call");
                }
                // Literals are `*`-typed (sema): hand back the box.
                Val::Any(boxed)
            }
            ExprKind::PropGet(recv, name) => {
                let p = self.as_any_ptr(recv);
                let key = self.const_string(name);
                let out = self.entry_alloca(cx.any_ty, "prop");
                let f = cx.runtime_fn(
                    "vs_any_get_prop",
                    None,
                    &[cx.ptr().into(), cx.ptr().into(), cx.ptr().into()],
                );
                self.cx
                    .builder
                    .build_call(f, &[p.into(), key.into(), out.into()], "")
                    .expect("call");
                self.unbox_any_ptr(out, e.ty)
            }
            ExprKind::PropSet(recv, name, value) => {
                let p = self.as_any_ptr(recv);
                let key = self.const_string(name);
                let v = self.expr(value);
                let vboxed = match v {
                    Val::Any(vp) => vp,
                    other => self.box_value(other),
                };
                let f = cx.runtime_fn(
                    "vs_any_set_prop",
                    None,
                    &[cx.ptr().into(), cx.ptr().into(), cx.ptr().into()],
                );
                self.cx
                    .builder
                    .build_call(f, &[p.into(), key.into(), vboxed.into()], "")
                    .expect("call");
                v
            }
            ExprKind::AnyIndexGet(recv, key) => {
                let p = self.as_any_ptr(recv);
                let k = self.as_any_ptr(key);
                let out = self.entry_alloca(cx.any_ty, "idx");
                let f = cx.runtime_fn(
                    "vs_any_index_get",
                    None,
                    &[cx.ptr().into(), cx.ptr().into(), cx.ptr().into()],
                );
                self.cx
                    .builder
                    .build_call(f, &[p.into(), k.into(), out.into()], "")
                    .expect("call");
                self.unbox_any_ptr(out, e.ty)
            }
            ExprKind::AnyIndexSet(recv, key, value) => {
                let p = self.as_any_ptr(recv);
                let k = self.as_any_ptr(key);
                let v = self.expr(value);
                let vboxed = match v {
                    Val::Any(vp) => vp,
                    other => self.box_value(other),
                };
                let f = cx.runtime_fn(
                    "vs_any_index_set",
                    None,
                    &[cx.ptr().into(), cx.ptr().into(), cx.ptr().into()],
                );
                self.cx
                    .builder
                    .build_call(f, &[p.into(), k.into(), vboxed.into()], "")
                    .expect("call");
                v
            }
            ExprKind::PropCall(recv, name, args) => {
                let p = self.as_any_ptr(recv);
                let key = self.const_string(name);
                let staged: Vec<Option<&mir::Expr>> = args.iter().map(Some).collect();
                let buf = self.stage_any_array_opt(&staged);
                let out = self.entry_alloca(cx.any_ty, "pcall");
                let f = cx.runtime_fn(
                    "vs_any_call_prop",
                    None,
                    &[
                        cx.ptr().into(),
                        cx.ptr().into(),
                        cx.context.i32_type().into(),
                        cx.ptr().into(),
                        cx.ptr().into(),
                    ],
                );
                let n = cx.context.i32_type().const_int(args.len() as u64, false);
                self.cx
                    .builder
                    .build_call(
                        f,
                        &[p.into(), key.into(), n.into(), buf.into(), out.into()],
                        "",
                    )
                    .expect("call");
                self.unbox_any_ptr(out, e.ty)
            }
            ExprKind::HasProp(key, obj) => {
                let k = self.as_any_ptr(key);
                let o = self.as_any_ptr(obj);
                let f = cx.runtime_fn(
                    "vs_any_has_prop",
                    Some(cx.context.i32_type().into()),
                    &[cx.ptr().into(), cx.ptr().into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[k.into(), o.into()], "hasp")
                    .expect("call");
                Val::Bool(
                    self.nonzero(
                        call.try_as_basic_value()
                            .basic()
                            .expect("value")
                            .into_int_value(),
                    ),
                )
            }
            ExprKind::DeleteProp(obj, name) => {
                let o = self.as_any_ptr(obj);
                let key = self.const_string(name);
                let f = cx.runtime_fn(
                    "vs_any_delete_prop",
                    Some(cx.context.i32_type().into()),
                    &[cx.ptr().into(), cx.ptr().into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[o.into(), key.into()], "delp")
                    .expect("call");
                Val::Bool(
                    self.nonzero(
                        call.try_as_basic_value()
                            .basic()
                            .expect("value")
                            .into_int_value(),
                    ),
                )
            }
            ExprKind::CallNative(nf, args) => self.call_native(*nf, args, e.ty),
            ExprKind::CaptureGet(i) => {
                let cell = self.capture_cell(*i);
                let copy = self.entry_alloca(cx.any_ty, "capval");
                let v = self
                    .cx
                    .builder
                    .build_load(cx.any_ty, cell, "capload")
                    .expect("load");
                self.cx.builder.build_store(copy, v).expect("store");
                self.unbox_any_ptr(copy, e.ty)
            }
            ExprKind::CaptureSet(i, value) => {
                let v = self.expr(value);
                let boxed = match v {
                    Val::Any(p) => p,
                    other => self.box_value(other),
                };
                let cell = self.capture_cell(*i);
                let loaded = self
                    .cx
                    .builder
                    .build_load(cx.any_ty, boxed, "capset")
                    .expect("load");
                self.cx.builder.build_store(cell, loaded).expect("store");
                v
            }
            ExprKind::Closure(fn_id) => {
                let wrapper = self.wrapper_of(*fn_id);
                let caps: Vec<mir::CapSrc> =
                    self.program.functions[fn_id.0 as usize].captures.clone();
                let n = caps.len() as u32;
                let arr = self.entry_alloca(cx.ptr().array_type(n.max(1)), "env");
                for (i, cap) in caps.iter().enumerate() {
                    let cell = match cap {
                        mir::CapSrc::ParentLocal(id) => {
                            let (slot, _, is_cell) = self.locals[id.0 as usize];
                            debug_assert!(is_cell, "captured local must be cell-backed");
                            self.cx
                                .builder
                                .build_load(cx.ptr(), slot, "cellp")
                                .expect("load")
                                .into_pointer_value()
                        }
                        mir::CapSrc::ParentCapture(j) => self.capture_cell(*j),
                    };
                    let slot = unsafe {
                        self.cx.builder.build_in_bounds_gep(
                            cx.ptr().array_type(n.max(1)),
                            arr,
                            &[
                                cx.context.i32_type().const_zero(),
                                cx.context.i32_type().const_int(i as u64, false),
                            ],
                            "envslot",
                        )
                    }
                    .expect("gep");
                    self.cx.builder.build_store(slot, cell).expect("store");
                }
                self.make_closure(wrapper, n, arr, cx.ptr().const_null())
            }
            ExprKind::FnValue(fn_id) => {
                let wrapper = self.wrapper_of(*fn_id);
                let null = cx.ptr().const_null();
                self.make_closure(wrapper, 0, null, null)
            }
            ExprKind::BuiltinValue(b) => {
                let wrapper = self.builtin_wrapper(*b);
                let null = cx.ptr().const_null();
                self.make_closure(wrapper, 0, null, null)
            }
            ExprKind::BoundMethod(recv, class, vslot) => {
                let obj = self.obj_operand(recv);
                self.null_check(obj);
                let wrapper = self.vwrapper_of(*class, *vslot);
                let null = cx.ptr().const_null();
                self.make_closure(wrapper, 0, null, obj)
            }
            ExprKind::CallFnValue {
                callee,
                this_arg,
                args,
                is_apply,
            } => self.call_fn_value(callee, this_arg.as_deref(), args, *is_apply, e.ty),
            ExprKind::EnumLen(v) => {
                let p = self.as_any_ptr(v);
                let f = cx.runtime_fn(
                    "vs_enum_len2",
                    Some(cx.context.i32_type().into()),
                    &[cx.ptr().into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[p.into()], "elen")
                    .expect("call");
                Val::Int(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_int_value(),
                )
            }
            ExprKind::EnumKey(v, i) | ExprKind::EnumValue(v, i) => {
                let is_key = matches!(e.kind, ExprKind::EnumKey(..));
                let p = self.as_any_ptr(v);
                let idx = match self.expr(i) {
                    Val::Int(x) | Val::UInt(x) => x,
                    _ => unreachable!("enum index"),
                };
                let out = self.entry_alloca(cx.any_ty, "enumout");
                let f = cx.runtime_fn(
                    if is_key {
                        "vs_enum_key2"
                    } else {
                        "vs_enum_value2"
                    },
                    None,
                    &[
                        cx.ptr().into(),
                        cx.context.i32_type().into(),
                        cx.ptr().into(),
                    ],
                );
                self.cx
                    .builder
                    .build_call(f, &[p.into(), idx.into(), out.into()], "")
                    .expect("call");
                Val::Any(out)
            }
            ExprKind::ArrayLit(elements) => {
                let staged: Vec<Option<&mir::Expr>> = elements.iter().map(|e| e.as_ref()).collect();
                let arr_ptr = self.stage_any_array_opt(&staged);
                let f = cx.runtime_fn(
                    "vs_array_new",
                    Some(cx.ptr().into()),
                    &[cx.context.i32_type().into(), cx.ptr().into()],
                );
                let n = cx
                    .context
                    .i32_type()
                    .const_int(elements.len() as u64, false);
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[n.into(), arr_ptr.into()], "arr")
                    .expect("call");
                Val::Arr(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            ExprKind::VectorLit(inst, elements) => {
                let staged: Vec<Option<&mir::Expr>> = elements.iter().map(Some).collect();
                let buf = self.stage_any_array_opt(&staged);
                let kind = self.vec_kind(*inst);
                let f = cx.runtime_fn(
                    "vs_vector_new",
                    Some(cx.ptr().into()),
                    &[
                        cx.context.i32_type().into(),
                        cx.context.i32_type().into(),
                        cx.context.i32_type().into(),
                        cx.ptr().into(),
                    ],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(
                        f,
                        &[
                            cx.context
                                .i32_type()
                                .const_int(u64::from(*inst), false)
                                .into(),
                            cx.context.i32_type().const_int(u64::from(kind), false).into(),
                            cx.context
                                .i32_type()
                                .const_int(elements.len() as u64, false)
                                .into(),
                            buf.into(),
                        ],
                        "vec",
                    )
                    .expect("call");
                Val::VecP(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            ExprKind::CallArr(m, recv, args) => self.call_arr(*m, recv, args),
            ExprKind::CallVec(m, recv, args) => self.call_vec(*m, recv, args, e.ty),
            ExprKind::SeqLen(recv) => {
                let (p, is_vec) = self.seq_ptr(recv);
                let name = if is_vec { "vs_vec_len" } else { "vs_arr_len" };
                let f = cx.runtime_fn(name, Some(cx.context.i32_type().into()), &[cx.ptr().into()]);
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[p.into()], "len")
                    .expect("call");
                Val::UInt(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_int_value(),
                )
            }
            ExprKind::SeqSetLen(recv, v) => {
                let (p, is_vec) = self.seq_ptr(recv);
                let n = match self.expr(v) {
                    Val::UInt(i) | Val::Int(i) => i,
                    _ => unreachable!("length operand"),
                };
                let name = if is_vec {
                    "vs_vec_set_len"
                } else {
                    "vs_arr_set_len"
                };
                let f = cx.runtime_fn(name, None, &[cx.ptr().into(), cx.context.i32_type().into()]);
                self.cx
                    .builder
                    .build_call(f, &[p.into(), n.into()], "")
                    .expect("call");
                Val::UInt(n)
            }
            ExprKind::SeqGet(recv, idx, unchecked) => {
                // P24: a preceding loop guard proved this index in range —
                // emit a raw load, no bounds check, so the loop vectorizes.
                if *unchecked
                    && let Some((_kind, ety, _stride)) = self.unboxed_vec(recv)
                {
                    let (p, _) = self.seq_ptr(recv);
                    let idx_f = self.num_arg(idx);
                    let (_len, data) = self.vec_header(p);
                    let i = self
                        .cx
                        .builder
                        .build_float_to_unsigned_int(idx_f, cx.context.i64_type(), "vi")
                        .expect("conv");
                    let ep = unsafe {
                        self.cx
                            .builder
                            .build_in_bounds_gep(ety, data, &[i], "vep")
                            .expect("gep")
                    };
                    let val = self.cx.builder.build_load(ety, ep, "vel").expect("load");
                    return match e.ty {
                        Ty::Number => Val::Num(val.into_float_value()),
                        Ty::Int => Val::Int(val.into_int_value()),
                        Ty::UInt => Val::UInt(val.into_int_value()),
                        _ => unreachable!("unboxed vector element type"),
                    };
                }
                // P23 fast path: an unboxed numeric Vector inlines the read as
                // a bounds-checked load, no runtime call and no boxing.
                if let Some((_kind, ety, _stride)) = self.unboxed_vec(recv) {
                    let (p, _) = self.seq_ptr(recv);
                    let idx_f = self.num_arg(idx);
                    let (len, data) = self.vec_header(p);
                    let f64t = cx.context.f64_type();
                    let len_f = self
                        .cx
                        .builder
                        .build_unsigned_int_to_float(len, f64t, "lenf")
                        .expect("conv");
                    let inb = self.idx_in_bounds(idx_f, len_f);
                    let fast = self.new_block("vget.fast");
                    let slow = self.new_block("vget.slow");
                    self.cx
                        .builder
                        .build_conditional_branch(inb, fast, slow)
                        .expect("br");
                    // Out of range → runtime raises RangeError (never returns).
                    self.cx.builder.position_at_end(slow);
                    let out = self.entry_alloca(cx.any_ty, "seqget");
                    let f = cx.runtime_fn(
                        "vs_vec_get",
                        None,
                        &[cx.ptr().into(), f64t.into(), cx.ptr().into()],
                    );
                    self.cx
                        .builder
                        .build_call(f, &[p.into(), idx_f.into(), out.into()], "")
                        .expect("call");
                    self.cx.builder.build_unreachable().expect("unreachable");
                    // In range → direct typed load.
                    self.cx.builder.position_at_end(fast);
                    let i = self
                        .cx
                        .builder
                        .build_float_to_unsigned_int(idx_f, cx.context.i64_type(), "vi")
                        .expect("conv");
                    let ep = unsafe {
                        self.cx
                            .builder
                            .build_in_bounds_gep(ety, data, &[i], "vep")
                            .expect("gep")
                    };
                    let val = self.cx.builder.build_load(ety, ep, "vel").expect("load");
                    match e.ty {
                        Ty::Number => Val::Num(val.into_float_value()),
                        Ty::Int => Val::Int(val.into_int_value()),
                        Ty::UInt => Val::UInt(val.into_int_value()),
                        _ => unreachable!("unboxed vector element type"),
                    }
                } else {
                    let (p, is_vec) = self.seq_ptr(recv);
                    let i = self.num_arg(idx);
                    let out = self.entry_alloca(cx.any_ty, "seqget");
                    let name = if is_vec { "vs_vec_get" } else { "vs_arr_get" };
                    let f = cx.runtime_fn(
                        name,
                        None,
                        &[
                            cx.ptr().into(),
                            cx.context.f64_type().into(),
                            cx.ptr().into(),
                        ],
                    );
                    self.cx
                        .builder
                        .build_call(f, &[p.into(), i.into(), out.into()], "")
                        .expect("call");
                    self.unbox_any_ptr(out, e.ty)
                }
            }
            ExprKind::SeqSet(recv, idx, v, unchecked) => {
                // P24: guard-proven in-range store — raw store, no bounds
                // check, so the loop vectorizes.
                if *unchecked
                    && let Some((kind, ety, _stride)) = self.unboxed_vec(recv)
                {
                    let value = self.expr(v);
                    let scalar = self.vec_scalar(value, kind);
                    let (p, _) = self.seq_ptr(recv);
                    let idx_f = self.num_arg(idx);
                    let (_len, data) = self.vec_header(p);
                    let i = self
                        .cx
                        .builder
                        .build_float_to_unsigned_int(idx_f, cx.context.i64_type(), "vi")
                        .expect("conv");
                    let ep = unsafe {
                        self.cx
                            .builder
                            .build_in_bounds_gep(ety, data, &[i], "vep")
                            .expect("gep")
                    };
                    self.cx.builder.build_store(ep, scalar).expect("store");
                    return value;
                }
                // P23 fast path: an unboxed numeric Vector inlines the in-range
                // store; append (i == len) and out-of-range fall to the runtime.
                if let Some((kind, ety, _stride)) = self.unboxed_vec(recv) {
                    let value = self.expr(v);
                    let scalar = self.vec_scalar(value, kind);
                    let (p, _) = self.seq_ptr(recv);
                    let idx_f = self.num_arg(idx);
                    let (len, data) = self.vec_header(p);
                    let f64t = cx.context.f64_type();
                    let len_f = self
                        .cx
                        .builder
                        .build_unsigned_int_to_float(len, f64t, "lenf")
                        .expect("conv");
                    let inb = self.idx_in_bounds(idx_f, len_f);
                    let fast = self.new_block("vset.fast");
                    let slow = self.new_block("vset.slow");
                    let cont = self.new_block("vset.cont");
                    self.cx
                        .builder
                        .build_conditional_branch(inb, fast, slow)
                        .expect("br");
                    // In range → direct typed store.
                    self.cx.builder.position_at_end(fast);
                    let i = self
                        .cx
                        .builder
                        .build_float_to_unsigned_int(idx_f, cx.context.i64_type(), "vi")
                        .expect("conv");
                    let ep = unsafe {
                        self.cx
                            .builder
                            .build_in_bounds_gep(ety, data, &[i], "vep")
                            .expect("gep")
                    };
                    self.cx.builder.build_store(ep, scalar).expect("store");
                    self.cx
                        .builder
                        .build_unconditional_branch(cont)
                        .expect("br");
                    // Append or out of range → runtime (boxes, may grow/raise).
                    self.cx.builder.position_at_end(slow);
                    let boxed = self.box_value(value);
                    let f = cx.runtime_fn(
                        "vs_vec_set",
                        None,
                        &[cx.ptr().into(), f64t.into(), cx.ptr().into()],
                    );
                    self.cx
                        .builder
                        .build_call(f, &[p.into(), idx_f.into(), boxed.into()], "")
                        .expect("call");
                    self.cx
                        .builder
                        .build_unconditional_branch(cont)
                        .expect("br");
                    self.cx.builder.position_at_end(cont);
                    value
                } else {
                    let (p, is_vec) = self.seq_ptr(recv);
                    let i = self.num_arg(idx);
                    let value = self.expr(v);
                    let boxed = match value {
                        Val::Any(p) => p,
                        other => self.box_value(other),
                    };
                    let name = if is_vec { "vs_vec_set" } else { "vs_arr_set" };
                    let f = cx.runtime_fn(
                        name,
                        None,
                        &[
                            cx.ptr().into(),
                            cx.context.f64_type().into(),
                            cx.ptr().into(),
                        ],
                    );
                    self.cx
                        .builder
                        .build_call(f, &[p.into(), i.into(), boxed.into()], "")
                        .expect("call");
                    value
                }
            }
            ExprKind::CaptureIncDec {
                slot,
                is_inc,
                is_prefix,
            } => {
                // Cells store boxed values: unbox → arith → rebox.
                let cell = self.capture_cell(*slot);
                let copy = self.entry_alloca(cx.any_ty, "cival");
                let loaded = self
                    .cx
                    .builder
                    .build_load(cx.any_ty, cell, "ci")
                    .expect("load");
                self.cx.builder.build_store(copy, loaded).expect("store");
                let old = self.unbox_any_ptr(copy, e.ty);
                let one_new = match old {
                    Val::Int(i) | Val::UInt(i) => {
                        let one = cx.context.i32_type().const_int(1, false);
                        let n = if *is_inc {
                            self.cx.builder.build_int_add(i, one, "inc")
                        } else {
                            self.cx.builder.build_int_sub(i, one, "dec")
                        }
                        .expect("arith");
                        match old {
                            Val::Int(_) => Val::Int(n),
                            _ => Val::UInt(n),
                        }
                    }
                    Val::Num(f) => {
                        let one = cx.context.f64_type().const_float(1.0);
                        Val::Num(
                            if *is_inc {
                                self.cx.builder.build_float_add(f, one, "inc")
                            } else {
                                self.cx.builder.build_float_sub(f, one, "dec")
                            }
                            .expect("arith"),
                        )
                    }
                    _ => unreachable!("numeric capture inc/dec"),
                };
                let boxed = self.box_value(one_new);
                let v = self
                    .cx
                    .builder
                    .build_load(cx.any_ty, boxed, "nb")
                    .expect("load");
                self.cx.builder.build_store(cell, v).expect("store");
                if *is_prefix { one_new } else { old }
            }
            ExprKind::StaticIncDec {
                class,
                index,
                is_inc,
                is_prefix,
            } => {
                let g = self.cx.classes[*class as usize].statics[*index];
                let ty = self.program.classes[*class as usize].statics[*index];
                self.inc_dec_at(g, ty, *is_inc, *is_prefix)
            }
            ExprKind::FieldIncDec {
                recv,
                class,
                slot,
                is_inc,
                is_prefix,
            } => {
                let obj = self.obj_operand(recv);
                self.null_check(obj);
                let (p, ty) = self.field_ptr(obj, *class, *slot);
                self.inc_dec_at(p, ty, *is_inc, *is_prefix)
            }
            ExprKind::This => Val::Obj(
                self.function
                    .get_nth_param(0)
                    .expect("this param")
                    .into_pointer_value(),
            ),
            ExprKind::New(class, args) => self.new_object(*class, args),
            ExprKind::FieldGet(recv, class, slot) => {
                let obj = self.obj_operand(recv);
                self.null_check(obj);
                let (p, ty) = self.field_ptr(obj, *class, *slot);
                let v = self
                    .cx
                    .builder
                    .build_load(cx.basic_ty(ty), p, "fld")
                    .expect("load");
                self.wrap_basic(v, ty)
            }
            ExprKind::FieldSet(recv, class, slot, value) => {
                let obj = self.obj_operand(recv);
                self.null_check(obj);
                let v = self.expr(value);
                let (p, ty) = self.field_ptr(obj, *class, *slot);
                let basic = self.ref_tolerant_basic(v, ty);
                self.cx.builder.build_store(p, basic).expect("store");
                self.wrap_basic(basic, ty)
            }
            ExprKind::CallVirtual {
                recv,
                class,
                vslot,
                args,
            } => self.call_virtual(recv, *class, *vslot, args, e.ty),
            ExprKind::CallIface {
                recv,
                iface,
                islot,
                ret,
                args,
            } => self.call_iface(recv, *iface, *islot, *ret, args, e.ty),
            ExprKind::CallDirect { fn_id, recv, args } => {
                let obj = self.obj_operand(recv);
                let mut argv: Vec<BasicMetadataValueEnum> = vec![obj.into()];
                for a in args {
                    let v = self.expr(a);
                    argv.push(self.materialize(v).into());
                }
                let call = self
                    .cx
                    .builder
                    .build_call(self.fns[fn_id.0 as usize], &argv, "direct")
                    .expect("call");
                match self.program.functions[fn_id.0 as usize].ret {
                    Ty::Void => Val::Void,
                    t => self.wrap_basic(call.try_as_basic_value().basic().expect("value"), t),
                }
            }
            ExprKind::StaticGet(class, index) => {
                let g = self.cx.classes[*class as usize].statics[*index];
                let ty = self.program.classes[*class as usize].statics[*index];
                let v = self
                    .cx
                    .builder
                    .build_load(cx.basic_ty(ty), g, "sget")
                    .expect("load");
                self.wrap_basic(v, ty)
            }
            ExprKind::StaticSet(class, index, value) => {
                let g = self.cx.classes[*class as usize].statics[*index];
                let ty = self.program.classes[*class as usize].statics[*index];
                let v = self.expr(value);
                let basic = self.ref_tolerant_basic(v, ty);
                self.cx.builder.build_store(g, basic).expect("store");
                self.wrap_basic(basic, ty)
            }
        }
    }

    /// Object-typed operand (null String literals flow in as `Str`).
    fn obj_operand(&mut self, e: &mir::Expr) -> PointerValue<'ctx> {
        match self.expr(e) {
            Val::Obj(p) | Val::Str(p) => p,
            _ => unreachable!("object operand"),
        }
    }

    fn null_check(&mut self, obj: PointerValue<'ctx>) {
        // Inline the null test rather than calling `vs_null_check`. The
        // fall-through (`ok`) path proves `obj` non-null to LLVM, so GVN drops
        // redundant and loop-invariant checks and the guarded field loads
        // become CSE/hoist-eligible — an opaque call would be re-run at every
        // access and pin every dependent load (measured: ~24 such calls per
        // nbody inner iteration). Null branches to a cold, noreturn throw.
        let cx = self.cx;
        let is_null = cx.builder.build_is_null(obj, "isnull").expect("isnull");
        let func = self.function;
        let npe = cx.context.append_basic_block(func, "npe");
        let ok = cx.context.append_basic_block(func, "npe.ok");
        cx.builder
            .build_conditional_branch(is_null, npe, ok)
            .expect("br");
        cx.builder.position_at_end(npe);
        let f = cx.runtime_fn("vs_null_throw", None, &[]);
        for name in ["noreturn", "cold"] {
            f.add_attribute(
                inkwell::attributes::AttributeLoc::Function,
                cx.context.create_enum_attribute(
                    inkwell::attributes::Attribute::get_named_enum_kind_id(name),
                    0,
                ),
            );
        }
        cx.builder.build_call(f, &[], "").expect("call");
        cx.builder.build_unreachable().expect("unreachable");
        cx.builder.position_at_end(ok);
    }

    /// Pointer to instance slot `slot` using the owning class's layout.
    fn field_ptr(
        &mut self,
        obj: PointerValue<'ctx>,
        class: u32,
        slot: usize,
    ) -> (PointerValue<'ctx>, Ty) {
        let art = &self.cx.classes[class as usize];
        let ty = self.program.classes[class as usize].slots[slot];
        let p = self
            .cx
            .builder
            .build_struct_gep(art.instance_ty, obj, (slot + 1) as u32, "fldp")
            .expect("gep");
        (p, ty)
    }

    /// Store-compatible basic value: reference kinds (String/Object/Iface)
    /// interchange freely as pointers (null literals, upcasts).
    fn ref_tolerant_basic(&mut self, v: Val<'ctx>, target: Ty) -> BasicValueEnum<'ctx> {
        match (target, v) {
            (
                Ty::Object(_)
                | Ty::Iface(_)
                | Ty::Array
                | Ty::Vector(_)
                | Ty::Function
                | Ty::RegExp
                | Ty::Date
                | Ty::Socket
                | Ty::Namespace,
                Val::Str(p),
            ) => p.into(),
            (
                Ty::String,
                Val::Obj(p)
                | Val::Arr(p)
                | Val::VecP(p)
                | Val::Fun(p)
                | Val::Reg(p)
                | Val::Dat(p)
                | Val::Sock(p)
                | Val::Ns(p),
            ) => p.into(),
            _ => self.materialize(v),
        }
    }

    fn new_object(&mut self, class: u32, args: &[mir::Expr]) -> Val<'ctx> {
        let cx = self.cx;
        let art = &cx.classes[class as usize];
        let (rtti, size, instance_ty) = (art.rtti, art.size, art.instance_ty);
        let alloc = cx.runtime_fn(
            "vs_alloc_object",
            Some(cx.ptr().into()),
            &[cx.ptr().into(), cx.context.i64_type().into()],
        );
        let call = self
            .cx
            .builder
            .build_call(
                alloc,
                &[
                    rtti.into(),
                    cx.context.i64_type().const_int(size, false).into(),
                ],
                "obj",
            )
            .expect("call");
        let obj = call
            .try_as_basic_value()
            .basic()
            .expect("value")
            .into_pointer_value();
        // Zeroed memory covers every default except Number = NaN
        // (SPECS §3.11).
        let slots: Vec<(usize, Ty)> = self.program.classes[class as usize]
            .slots
            .iter()
            .copied()
            .enumerate()
            .collect();
        for (slot, ty) in slots {
            if ty == Ty::Number {
                let p = self
                    .cx
                    .builder
                    .build_struct_gep(instance_ty, obj, (slot + 1) as u32, "nanp")
                    .expect("gep");
                self.cx
                    .builder
                    .build_store(p, cx.context.f64_type().const_float(f64::NAN))
                    .expect("store");
            }
        }
        // Run the constructor chain.
        let ctor = self.program.classes[class as usize].ctor;
        let mut argv: Vec<BasicMetadataValueEnum> = vec![obj.into()];
        for a in args {
            let v = self.expr(a);
            argv.push(self.materialize(v).into());
        }
        self.cx
            .builder
            .build_call(self.fns[ctor.0 as usize], &argv, "")
            .expect("call");
        Val::Obj(obj)
    }

    /// Builds the LLVM function type of a method from its MIR definition.
    fn method_fn_type(&self, fn_id: mir::FnId) -> inkwell::types::FunctionType<'ctx> {
        let cx = self.cx;
        let f = &self.program.functions[fn_id.0 as usize];
        let mut params: Vec<BasicMetadataTypeEnum> = vec![cx.ptr().into()];
        params.extend(
            f.locals[..f.param_count]
                .iter()
                .map(|&t| BasicMetadataTypeEnum::from(cx.basic_ty(t))),
        );
        match f.ret {
            Ty::Void => cx.context.void_type().fn_type(&params, false),
            t => cx.basic_ty(t).fn_type(&params, false),
        }
    }

    fn call_virtual(
        &mut self,
        recv: &mir::Expr,
        class: u32,
        vslot: usize,
        args: &[mir::Expr],
        expr_ty: Ty,
    ) -> Val<'ctx> {
        let cx = self.cx;
        let obj = self.obj_operand(recv);
        self.null_check(obj);
        // Header word 0 = descriptor pointer; vtable is rtti field 8.
        let desc = self
            .cx
            .builder
            .build_load(cx.ptr(), obj, "desc")
            .expect("load")
            .into_pointer_value();
        let rtti_ty = cx.classes[class as usize].rtti_ty;
        let vt = self
            .cx
            .builder
            .build_struct_gep(rtti_ty, desc, 11, "vt")
            .expect("gep");
        let slot_ptr = unsafe {
            self.cx.builder.build_in_bounds_gep(
                cx.ptr().array_type(0),
                vt,
                &[
                    cx.context.i32_type().const_zero(),
                    cx.context.i32_type().const_int(vslot as u64, false),
                ],
                "vslot",
            )
        }
        .expect("gep");
        let fptr = self
            .cx
            .builder
            .build_load(cx.ptr(), slot_ptr, "vfn")
            .expect("load")
            .into_pointer_value();
        let target = self.program.classes[class as usize].vtable[vslot];
        let fn_ty = self.method_fn_type(target);
        let mut argv: Vec<BasicMetadataValueEnum> = vec![obj.into()];
        let mut first_arg: Option<Val> = None;
        for a in args {
            let v = self.expr(a);
            first_arg.get_or_insert(v);
            argv.push(self.materialize(v).into());
        }
        let call = self
            .cx
            .builder
            .build_indirect_call(fn_ty, fptr, &argv, "vcall")
            .expect("call");
        let callee_ret = self.program.functions[target.0 as usize].ret;
        if callee_ret == Ty::Void {
            // Setter calls: the assignment expression's value is the
            // assigned operand.
            if expr_ty != Ty::Void {
                return first_arg.unwrap_or(Val::Void);
            }
            return Val::Void;
        }
        self.wrap_basic(
            call.try_as_basic_value().basic().expect("value"),
            callee_ret,
        )
    }

    fn call_iface(
        &mut self,
        recv: &mir::Expr,
        iface: u32,
        islot: usize,
        ret: Ty,
        args: &[mir::Expr],
        expr_ty: Ty,
    ) -> Val<'ctx> {
        let cx = self.cx;
        let obj = self.obj_operand(recv);
        let lookup = cx.runtime_fn(
            "vs_iface_table",
            Some(cx.ptr().into()),
            &[cx.ptr().into(), cx.context.i32_type().into()],
        );
        let table = self
            .cx
            .builder
            .build_call(
                lookup,
                &[
                    obj.into(),
                    cx.context
                        .i32_type()
                        .const_int(u64::from(iface), false)
                        .into(),
                ],
                "itab",
            )
            .expect("call")
            .try_as_basic_value()
            .basic()
            .expect("value")
            .into_pointer_value();
        let slot_ptr = unsafe {
            self.cx.builder.build_in_bounds_gep(
                cx.ptr(),
                table,
                &[cx.context.i32_type().const_int(islot as u64, false)],
                "islot",
            )
        }
        .expect("gep");
        let fptr = self
            .cx
            .builder
            .build_load(cx.ptr(), slot_ptr, "ifn")
            .expect("load")
            .into_pointer_value();
        // Function type: this + coerced arg types, declared return.
        let mut params: Vec<BasicMetadataTypeEnum> = vec![cx.ptr().into()];
        let mut argv: Vec<BasicMetadataValueEnum> = vec![obj.into()];
        let mut first_arg: Option<Val> = None;
        for a in args {
            let v = self.expr(a);
            first_arg.get_or_insert(v);
            params.push(BasicMetadataTypeEnum::from(cx.basic_ty(a.ty)));
            argv.push(self.materialize(v).into());
        }
        let fn_ty = match ret {
            Ty::Void => cx.context.void_type().fn_type(&params, false),
            t => cx.basic_ty(t).fn_type(&params, false),
        };
        let call = self
            .cx
            .builder
            .build_indirect_call(fn_ty, fptr, &argv, "icall")
            .expect("call");
        if ret == Ty::Void {
            if expr_ty != Ty::Void {
                return first_arg.unwrap_or(Val::Void);
            }
            return Val::Void;
        }
        self.wrap_basic(call.try_as_basic_value().basic().expect("value"), ret)
    }

    fn load_local(&mut self, id: mir::LocalId) -> Val<'ctx> {
        let (slot, ty, is_cell) = self.locals[id.0 as usize];
        let cx = self.cx;
        if is_cell {
            let cell = self
                .cx
                .builder
                .build_load(cx.ptr(), slot, "cellp")
                .expect("load")
                .into_pointer_value();
            // Copy the boxed value out so later writes don't alias.
            let copy = self.entry_alloca(cx.any_ty, "cellval");
            let v = self
                .cx
                .builder
                .build_load(cx.any_ty, cell, "cellload")
                .expect("load");
            self.cx.builder.build_store(copy, v).expect("store");
            return self.unbox_any_ptr(copy, ty);
        }
        match ty {
            Ty::Any => {
                // Copy the box so later writes to the local don't alias.
                let copy = self.entry_alloca(cx.any_ty, "anyget");
                let v = self
                    .cx
                    .builder
                    .build_load(cx.any_ty, slot, "load")
                    .expect("load");
                self.cx.builder.build_store(copy, v).expect("store");
                Val::Any(copy)
            }
            _ => {
                let v = self
                    .cx
                    .builder
                    .build_load(cx.basic_ty(ty), slot, "load")
                    .expect("load");
                self.wrap_basic(v, ty)
            }
        }
    }

    fn store_local(&mut self, id: mir::LocalId, v: Val<'ctx>) {
        let (slot, ty, is_cell) = self.locals[id.0 as usize];
        if is_cell {
            let boxed = match v {
                Val::Any(p) => p,
                other => self.box_value(other),
            };
            let cell = self
                .cx
                .builder
                .build_load(self.cx.ptr(), slot, "cellp")
                .expect("load")
                .into_pointer_value();
            let value = self
                .cx
                .builder
                .build_load(self.cx.any_ty, boxed, "boxval")
                .expect("load");
            self.cx.builder.build_store(cell, value).expect("store");
            return;
        }
        // Reference kinds (String/Object/Iface) interchange as pointers
        // (null literals, upcasts share representation).
        if matches!(
            ty,
            Ty::Object(_)
                | Ty::Iface(_)
                | Ty::String
                | Ty::Array
                | Ty::Vector(_)
                | Ty::Function
                | Ty::RegExp
                | Ty::Date
                | Ty::Socket
                | Ty::Namespace
        ) && let Val::Str(p)
        | Val::Obj(p)
        | Val::Arr(p)
        | Val::VecP(p)
        | Val::Fun(p)
        | Val::Reg(p)
        | Val::Dat(p)
        | Val::Sock(p)
        | Val::Ns(p) = v
        {
            self.cx.builder.build_store(slot, p).expect("store");
            return;
        }
        match (ty, v) {
            (Ty::Any, Val::Any(p)) => {
                let value = self
                    .cx
                    .builder
                    .build_load(self.cx.any_ty, p, "anyval")
                    .expect("load");
                self.cx.builder.build_store(slot, value).expect("store");
            }
            _ => {
                let basic = self.materialize(v);
                self.cx.builder.build_store(slot, basic).expect("store");
            }
        }
    }

    fn materialize(&mut self, v: Val<'ctx>) -> BasicValueEnum<'ctx> {
        match v {
            Val::Int(v) | Val::UInt(v) | Val::Bool(v) => v.into(),
            Val::Num(v) => v.into(),
            Val::Str(p)
            | Val::Obj(p)
            | Val::Arr(p)
            | Val::VecP(p)
            | Val::Fun(p)
            | Val::Reg(p)
            | Val::Dat(p)
            | Val::Sock(p)
            | Val::Ns(p) => p.into(),
            Val::Any(p) => self
                .cx
                .builder
                .build_load(self.cx.any_ty, p, "anyval")
                .expect("load"),
            Val::Void => unreachable!("void as value"),
        }
    }

    fn wrap_basic(&mut self, v: BasicValueEnum<'ctx>, ty: Ty) -> Val<'ctx> {
        match ty {
            Ty::Int => Val::Int(v.into_int_value()),
            Ty::UInt => Val::UInt(v.into_int_value()),
            Ty::Number => Val::Num(v.into_float_value()),
            Ty::Boolean => Val::Bool(v.into_int_value()),
            Ty::String => Val::Str(v.into_pointer_value()),
            Ty::Object(_) | Ty::Iface(_) => Val::Obj(v.into_pointer_value()),
            Ty::Array => Val::Arr(v.into_pointer_value()),
            Ty::Vector(_) => Val::VecP(v.into_pointer_value()),
            Ty::Function => Val::Fun(v.into_pointer_value()),
            Ty::RegExp => Val::Reg(v.into_pointer_value()),
            Ty::Date => Val::Dat(v.into_pointer_value()),
            Ty::Socket => Val::Sock(v.into_pointer_value()),
            Ty::Namespace => Val::Ns(v.into_pointer_value()),
            Ty::Any => {
                let slot = self.entry_alloca(self.cx.any_ty, "anyv");
                self.cx.builder.build_store(slot, v).expect("store");
                Val::Any(slot)
            }
            Ty::Void => Val::Void,
        }
    }

    // --- conversions -----------------------------------------------------------

    fn convert(&mut self, conv: Conv, operand: &mir::Expr) -> Val<'ctx> {
        let cx = self.cx;
        let v = self.expr(operand);
        match conv {
            Conv::ToInt | Conv::ToUInt => {
                let wrap = |iv: IntValue<'ctx>| {
                    if conv == Conv::ToInt {
                        Val::Int(iv)
                    } else {
                        Val::UInt(iv)
                    }
                };
                match v {
                    // int↔uint reinterpret the same 32 bits (§9.5/§9.6).
                    Val::Int(i) | Val::UInt(i) => wrap(i),
                    Val::Num(f) => {
                        let v = self.convert_num_to_int(f, conv);
                        let (Val::Int(iv) | Val::UInt(iv)) = v else {
                            unreachable!("num-to-int shape")
                        };
                        wrap(iv)
                    }
                    Val::Any(p) => {
                        let n = self.any_to_number(p);
                        self.convert_num_to_int(n, conv)
                    }
                    _ => unreachable!("sema-checked conversion"),
                }
            }
            Conv::ToNumber => match v {
                Val::Num(f) => Val::Num(f),
                Val::Int(i) => Val::Num(
                    self.cx
                        .builder
                        .build_signed_int_to_float(i, cx.context.f64_type(), "sitofp")
                        .expect("conv"),
                ),
                Val::UInt(i) => Val::Num(
                    self.cx
                        .builder
                        .build_unsigned_int_to_float(i, cx.context.f64_type(), "uitofp")
                        .expect("conv"),
                ),
                Val::Any(p) => Val::Num(self.any_to_number(p)),
                Val::Str(p) => {
                    let f = cx.runtime_fn(
                        "vs_string_to_number",
                        Some(cx.context.f64_type().into()),
                        &[cx.ptr().into()],
                    );
                    let call = self
                        .cx
                        .builder
                        .build_call(f, &[p.into()], "s2n")
                        .expect("call");
                    Val::Num(
                        call.try_as_basic_value()
                            .basic()
                            .expect("value")
                            .into_float_value(),
                    )
                }
                _ => unreachable!("sema-checked conversion"),
            },
            Conv::ToBoolean => Val::Bool(self.truthy(v)),
            Conv::ToString => Val::Str(self.stringify(v)),
            Conv::ToAny => Val::Any(self.box_value(v)),
            Conv::AnyToString => {
                let Val::Any(p) = v else {
                    unreachable!("AnyToString operand")
                };
                let f = cx.runtime_fn(
                    "vs_any_coerce_string",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[p.into()], "coerce_s")
                    .expect("call");
                Val::Str(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            Conv::AnyToObject(class) => {
                let Val::Any(p) = v else {
                    unreachable!("AnyToObject operand")
                };
                let f = cx.runtime_fn(
                    "vs_any_coerce_class",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into(), cx.ptr().into()],
                );
                let rtti = cx.classes[class as usize].rtti;
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[p.into(), rtti.into()], "coerce_o")
                    .expect("call");
                Val::Obj(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            Conv::AnyToArray => {
                let Val::Any(p) = v else {
                    unreachable!("AnyToArray operand")
                };
                let f = cx.runtime_fn(
                    "vs_any_coerce_array",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[p.into()], "coerce_arr")
                    .expect("call");
                Val::Arr(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            Conv::AnyToVector(inst) => {
                let Val::Any(p) = v else {
                    unreachable!("AnyToVector operand")
                };
                let f = cx.runtime_fn(
                    "vs_any_coerce_vector",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into(), cx.context.i32_type().into()],
                );
                let id = cx.context.i32_type().const_int(u64::from(inst), false);
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[p.into(), id.into()], "coerce_vec")
                    .expect("call");
                Val::VecP(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            Conv::AnyToRegExp => {
                let Val::Any(p) = v else {
                    unreachable!("AnyToRegExp operand")
                };
                let f = cx.runtime_fn(
                    "vs_any_to_regexp",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[p.into()], "coerce_re")
                    .expect("call");
                Val::Reg(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            Conv::AnyToSocket => {
                let Val::Any(p) = v else {
                    unreachable!("AnyToSocket operand")
                };
                let f = cx.runtime_fn(
                    "vs_any_to_socket",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[p.into()], "coerce_sk")
                    .expect("call");
                Val::Sock(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            Conv::AnyToDate => {
                let Val::Any(p) = v else {
                    unreachable!("AnyToDate operand")
                };
                let f = cx.runtime_fn("vs_any_to_date", Some(cx.ptr().into()), &[cx.ptr().into()]);
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[p.into()], "coerce_d")
                    .expect("call");
                Val::Dat(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            Conv::AnyToIface(iface) => {
                let Val::Any(p) = v else {
                    unreachable!("AnyToIface operand")
                };
                let f = cx.runtime_fn(
                    "vs_any_coerce_iface",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into(), cx.context.i32_type().into()],
                );
                let id = cx.context.i32_type().const_int(u64::from(iface), false);
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[p.into(), id.into()], "coerce_i")
                    .expect("call");
                Val::Obj(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
        }
    }

    /// ToInt32/ToUint32 (§9.5/§9.6) with an inline fast path: a double
    /// already inside the target range truncates with one instruction;
    /// NaN/±Inf/out-of-range take the runtime's modular slow path. The
    /// bounds are exclusive-of-boundary so every value that branches to
    /// `fptosi`/`fptoui` truncates in range (no poison).
    fn convert_num_to_int(&mut self, f: FloatValue<'ctx>, conv: Conv) -> Val<'ctx> {
        let cx = self.cx;
        let f64t = cx.context.f64_type();
        let i32t = cx.context.i32_type();
        let b = &cx.builder;
        let (name, lo, hi) = if conv == Conv::ToInt {
            ("vs_f64_to_int32", -2147483649.0, 2147483648.0)
        } else {
            ("vs_f64_to_uint32", -1.0, 4294967296.0)
        };
        // in-range = f > lo && f < hi (ordered compares: NaN fails both).
        let gt = b
            .build_float_compare(FloatPredicate::OGT, f, f64t.const_float(lo), "toi.gt")
            .expect("cmp");
        let lt = b
            .build_float_compare(FloatPredicate::OLT, f, f64t.const_float(hi), "toi.lt")
            .expect("cmp");
        let in_range = b.build_and(gt, lt, "toi.in").expect("and");
        let fast_bb = self.new_block("toi.fast");
        let slow_bb = self.new_block("toi.slow");
        let end_bb = self.new_block("toi.end");
        b.build_conditional_branch(in_range, fast_bb, slow_bb)
            .expect("br");
        b.position_at_end(fast_bb);
        let fast = if conv == Conv::ToInt {
            b.build_float_to_signed_int(f, i32t, "toi.trunc")
                .expect("fptosi")
        } else {
            b.build_float_to_unsigned_int(f, i32t, "toi.trunc")
                .expect("fptoui")
        };
        b.build_unconditional_branch(end_bb).expect("br");
        b.position_at_end(slow_bb);
        let rf = cx.runtime_fn(name, Some(i32t.into()), &[f64t.into()]);
        let call = b.build_call(rf, &[f.into()], "toi.call").expect("call");
        let slow = call
            .try_as_basic_value()
            .basic()
            .expect("value")
            .into_int_value();
        b.build_unconditional_branch(end_bb).expect("br");
        b.position_at_end(end_bb);
        let phi = b.build_phi(i32t, "toi").expect("phi");
        phi.add_incoming(&[(&fast, fast_bb), (&slow, slow_bb)]);
        let iv = phi.as_basic_value().into_int_value();
        if conv == Conv::ToInt {
            Val::Int(iv)
        } else {
            Val::UInt(iv)
        }
    }

    fn any_to_number(&mut self, p: PointerValue<'ctx>) -> FloatValue<'ctx> {
        let cx = self.cx;
        let f = cx.runtime_fn(
            "vs_any_to_number",
            Some(cx.context.f64_type().into()),
            &[cx.ptr().into()],
        );
        let call = self
            .cx
            .builder
            .build_call(f, &[p.into()], "a2n")
            .expect("call");
        call.try_as_basic_value()
            .basic()
            .expect("value")
            .into_float_value()
    }

    /// ToString for every value shape (ES3 §9.8).
    fn stringify(&mut self, v: Val<'ctx>) -> PointerValue<'ctx> {
        let cx = self.cx;
        let call = match v {
            Val::Str(p) => return p,
            Val::Obj(p) => {
                let rf = cx.runtime_fn(
                    "vs_object_to_string",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into()],
                );
                self.cx.builder.build_call(rf, &[p.into()], "o2s")
            }
            Val::Arr(p) => {
                let rf = cx.runtime_fn(
                    "vs_arr_to_string",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into()],
                );
                self.cx.builder.build_call(rf, &[p.into()], "arr2s")
            }
            Val::VecP(p) => {
                let rf = cx.runtime_fn(
                    "vs_vec_to_string",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into()],
                );
                self.cx.builder.build_call(rf, &[p.into()], "vec2s")
            }
            Val::Reg(p) => {
                let rf = cx.runtime_fn(
                    "vs_regexp_display",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into()],
                );
                self.cx.builder.build_call(rf, &[p.into()], "re2s")
            }
            Val::Ns(p) => {
                let rf = cx.runtime_fn(
                    "vs_namespace_uri",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into()],
                );
                self.cx.builder.build_call(rf, &[p.into()], "ns2s")
            }
            Val::Sock(_) => {
                let lit = self
                    .cx
                    .builder
                    .build_global_string_ptr("[object Socket]", "sockstr")
                    .expect("global");
                let len = cx.context.i32_type().const_int(15, false);
                let rf = cx.runtime_fn(
                    "vs_string_from_utf8",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into(), cx.context.i32_type().into()],
                );
                self.cx
                    .builder
                    .build_call(rf, &[lit.as_pointer_value().into(), len.into()], "s2s")
            }
            Val::Dat(p) => {
                let rf = cx.runtime_fn(
                    "vs_date_to_string",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into(), cx.context.i32_type().into()],
                );
                self.cx.builder.build_call(
                    rf,
                    &[p.into(), cx.context.i32_type().const_zero().into()],
                    "d2s",
                )
            }
            Val::Fun(_) => {
                let lit = self
                    .cx
                    .builder
                    .build_global_string_ptr("function Function() {}", "fnstr")
                    .expect("global");
                let len = cx.context.i32_type().const_int(22, false);
                let rf = cx.runtime_fn(
                    "vs_string_from_utf8",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into(), cx.context.i32_type().into()],
                );
                self.cx
                    .builder
                    .build_call(rf, &[lit.as_pointer_value().into(), len.into()], "f2s")
            }
            Val::Num(f) => {
                let rf = cx.runtime_fn(
                    "vs_num_to_string",
                    Some(cx.ptr().into()),
                    &[cx.context.f64_type().into()],
                );
                self.cx.builder.build_call(rf, &[f.into()], "n2s")
            }
            Val::Int(i) => {
                let f = self
                    .cx
                    .builder
                    .build_signed_int_to_float(i, cx.context.f64_type(), "sitofp")
                    .expect("conv");
                let rf = cx.runtime_fn(
                    "vs_num_to_string",
                    Some(cx.ptr().into()),
                    &[cx.context.f64_type().into()],
                );
                self.cx.builder.build_call(rf, &[f.into()], "n2s")
            }
            Val::UInt(i) => {
                let f = self
                    .cx
                    .builder
                    .build_unsigned_int_to_float(i, cx.context.f64_type(), "uitofp")
                    .expect("conv");
                let rf = cx.runtime_fn(
                    "vs_num_to_string",
                    Some(cx.ptr().into()),
                    &[cx.context.f64_type().into()],
                );
                self.cx.builder.build_call(rf, &[f.into()], "n2s")
            }
            Val::Bool(b) => {
                let z = self
                    .cx
                    .builder
                    .build_int_z_extend(b, cx.context.i32_type(), "zext")
                    .expect("zext");
                let rf = cx.runtime_fn(
                    "vs_bool_to_string",
                    Some(cx.ptr().into()),
                    &[cx.context.i32_type().into()],
                );
                self.cx.builder.build_call(rf, &[z.into()], "b2s")
            }
            Val::Any(p) => {
                let rf = cx.runtime_fn(
                    "vs_any_to_string",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into()],
                );
                self.cx.builder.build_call(rf, &[p.into()], "a2s")
            }
            Val::Void => unreachable!("void as value"),
        }
        .expect("call");
        call.try_as_basic_value()
            .basic()
            .expect("value")
            .into_pointer_value()
    }

    /// Boxes a value into a fresh `{i32, i64}` alloca (ABI: runtime tags).
    fn box_value(&mut self, v: Val<'ctx>) -> PointerValue<'ctx> {
        let cx = self.cx;
        if let Val::Any(p) = v {
            return p;
        }
        let slot = self.entry_alloca(cx.any_ty, "box");
        let i32t = cx.context.i32_type();
        let i64t = cx.context.i64_type();
        let (tag_v, payload): (IntValue, IntValue) = match v {
            Val::Int(i) => (
                i32t.const_int(u64::from(tag::INT), false),
                self.cx
                    .builder
                    .build_int_s_extend(i, i64t, "sext")
                    .expect("sext"),
            ),
            Val::UInt(i) => (
                i32t.const_int(u64::from(tag::UINT), false),
                self.cx
                    .builder
                    .build_int_z_extend(i, i64t, "zext")
                    .expect("zext"),
            ),
            Val::Num(f) => (
                i32t.const_int(u64::from(tag::NUMBER), false),
                self.cx
                    .builder
                    .build_bit_cast(f, i64t, "bits")
                    .expect("cast")
                    .into_int_value(),
            ),
            Val::Bool(b) => (
                i32t.const_int(u64::from(tag::BOOLEAN), false),
                self.cx
                    .builder
                    .build_int_z_extend(b, i64t, "zext")
                    .expect("zext"),
            ),
            Val::Str(p) => {
                // null String boxes as the null value, not a String box.
                let is_null = self.cx.builder.build_is_null(p, "isnull").expect("isnull");
                let t = self
                    .cx
                    .builder
                    .build_select(
                        is_null,
                        i32t.const_int(u64::from(tag::NULL), false),
                        i32t.const_int(u64::from(tag::STRING), false),
                        "tag",
                    )
                    .expect("select")
                    .into_int_value();
                let bits = self
                    .cx
                    .builder
                    .build_ptr_to_int(p, i64t, "p2i")
                    .expect("cast");
                (t, bits)
            }
            Val::Obj(p)
            | Val::Arr(p)
            | Val::VecP(p)
            | Val::Fun(p)
            | Val::Reg(p)
            | Val::Dat(p)
            | Val::Sock(p)
            | Val::Ns(p) => {
                let full_tag = match v {
                    Val::Obj(_) => tag::OBJECT,
                    Val::Arr(_) => tag::ARRAY,
                    Val::Fun(_) => tag::FUNCTION,
                    Val::Reg(_) => tag::REGEXP,
                    Val::Dat(_) => tag::DATE,
                    Val::Sock(_) => tag::SOCKET,
                    Val::Ns(_) => tag::NAMESPACE,
                    _ => tag::VECTOR,
                };
                // null pointers box as the null value.
                let is_null = self.cx.builder.build_is_null(p, "isnull").expect("isnull");
                let t = self
                    .cx
                    .builder
                    .build_select(
                        is_null,
                        i32t.const_int(u64::from(tag::NULL), false),
                        i32t.const_int(u64::from(full_tag), false),
                        "tag",
                    )
                    .expect("select")
                    .into_int_value();
                let bits = self
                    .cx
                    .builder
                    .build_ptr_to_int(p, i64t, "p2i")
                    .expect("cast");
                (t, bits)
            }
            Val::Any(_) | Val::Void => unreachable!(),
        };
        let tag_ptr = self
            .cx
            .builder
            .build_struct_gep(cx.any_ty, slot, 0, "tagp")
            .expect("gep");
        self.cx.builder.build_store(tag_ptr, tag_v).expect("store");
        let data_ptr = self
            .cx
            .builder
            .build_struct_gep(cx.any_ty, slot, 1, "datap")
            .expect("gep");
        self.cx
            .builder
            .build_store(data_ptr, payload)
            .expect("store");
        slot
    }

    /// Truthiness (ES3 §9.2) of any value shape, as `i1`.
    fn truthy(&mut self, v: Val<'ctx>) -> IntValue<'ctx> {
        let cx = self.cx;
        match v {
            Val::Bool(b) => b,
            Val::Int(i) | Val::UInt(i) => self
                .cx
                .builder
                .build_int_compare(
                    IntPredicate::NE,
                    i,
                    cx.context.i32_type().const_zero(),
                    "tob",
                )
                .expect("cmp"),
            // ONE is false for NaN and ±0 — exactly ToBoolean(Number).
            Val::Num(f) => self
                .cx
                .builder
                .build_float_compare(
                    FloatPredicate::ONE,
                    f,
                    cx.context.f64_type().const_zero(),
                    "tob",
                )
                .expect("cmp"),
            Val::Obj(p)
            | Val::Arr(p)
            | Val::VecP(p)
            | Val::Fun(p)
            | Val::Reg(p)
            | Val::Dat(p)
            | Val::Sock(p)
            | Val::Ns(p) => self
                .cx
                .builder
                .build_is_not_null(p, "objtrue")
                .expect("isnull"),
            Val::Str(_) | Val::Any(_) => {
                let p = match v {
                    Val::Any(p) => p,
                    v => self.box_value(v),
                };
                let f = cx.runtime_fn(
                    "vs_any_truthy",
                    Some(cx.context.i32_type().into()),
                    &[cx.ptr().into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[p.into()], "truthy")
                    .expect("call");
                self.nonzero(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_int_value(),
                )
            }
            Val::Void => unreachable!("void as value"),
        }
    }

    fn nonzero(&self, v: IntValue<'ctx>) -> IntValue<'ctx> {
        self.cx
            .builder
            .build_int_compare(IntPredicate::NE, v, v.get_type().const_zero(), "nz")
            .expect("cmp")
    }

    fn as_bool(&mut self, v: Val<'ctx>) -> IntValue<'ctx> {
        match v {
            Val::Bool(b) => b,
            other => self.truthy(other),
        }
    }

    /// Boxes if needed and yields the box pointer.
    fn as_any_ptr(&mut self, e: &mir::Expr) -> PointerValue<'ctx> {
        let v = self.expr(e);
        match v {
            Val::Any(p) => p,
            other => self.box_value(other),
        }
    }

    // --- operators ----------------------------------------------------------------

    fn unary(&mut self, op: UnOp, operand: &mir::Expr) -> Val<'ctx> {
        let cx = self.cx;
        let v = self.expr(operand);
        match op {
            UnOp::Not => {
                let b = self.as_bool(v);
                Val::Bool(self.cx.builder.build_not(b, "not").expect("not"))
            }
            UnOp::BitNot => {
                let Val::Int(i) = v else {
                    unreachable!("BitNot operand")
                };
                Val::Int(self.cx.builder.build_not(i, "bnot").expect("not"))
            }
            UnOp::Neg => match v {
                Val::Num(f) => Val::Num(self.cx.builder.build_float_neg(f, "neg").expect("neg")),
                // negate_i wraps (AVM2 OP_negate_i).
                Val::Int(i) => Val::Int(self.cx.builder.build_int_neg(i, "negi").expect("neg")),
                _ => unreachable!("Neg operand"),
            },
            UnOp::TypeOf => {
                let p = match v {
                    Val::Any(p) => p,
                    other => self.box_value(other),
                };
                let f = cx.runtime_fn("vs_any_typeof", Some(cx.ptr().into()), &[cx.ptr().into()]);
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[p.into()], "typeof")
                    .expect("call");
                Val::Str(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
        }
    }

    /// Increment/decrement of any addressable numeric slot.
    fn inc_dec_at(
        &mut self,
        slot: PointerValue<'ctx>,
        ty: Ty,
        is_inc: bool,
        is_prefix: bool,
    ) -> Val<'ctx> {
        let loaded = self
            .cx
            .builder
            .build_load(self.cx.basic_ty(ty), slot, "old")
            .expect("load");
        let old = self.wrap_basic(loaded, ty);
        let new = match old {
            Val::Int(i) | Val::UInt(i) => {
                let one = self.cx.context.i32_type().const_int(1, false);
                let n = if is_inc {
                    self.cx.builder.build_int_add(i, one, "inc")
                } else {
                    self.cx.builder.build_int_sub(i, one, "dec")
                }
                .expect("arith");
                match old {
                    Val::Int(_) => Val::Int(n),
                    _ => Val::UInt(n),
                }
            }
            Val::Num(f) => {
                let one = self.cx.context.f64_type().const_float(1.0);
                Val::Num(
                    if is_inc {
                        self.cx.builder.build_float_add(f, one, "inc")
                    } else {
                        self.cx.builder.build_float_sub(f, one, "dec")
                    }
                    .expect("arith"),
                )
            }
            _ => unreachable!("numeric inc/dec target"),
        };
        let basic = self.materialize(new);
        self.cx.builder.build_store(slot, basic).expect("store");
        if is_prefix { new } else { old }
    }

    fn inc_dec(&mut self, target: mir::LocalId, is_inc: bool, is_prefix: bool) -> Val<'ctx> {
        let old = self.load_local(target);
        let new = match old {
            Val::Int(i) | Val::UInt(i) => {
                let one = self.cx.context.i32_type().const_int(1, false);
                let n = if is_inc {
                    self.cx.builder.build_int_add(i, one, "inc")
                } else {
                    self.cx.builder.build_int_sub(i, one, "dec")
                }
                .expect("arith");
                match old {
                    Val::Int(_) => Val::Int(n),
                    _ => Val::UInt(n),
                }
            }
            Val::Num(f) => {
                let one = self.cx.context.f64_type().const_float(1.0);
                Val::Num(
                    if is_inc {
                        self.cx.builder.build_float_add(f, one, "inc")
                    } else {
                        self.cx.builder.build_float_sub(f, one, "dec")
                    }
                    .expect("arith"),
                )
            }
            _ => unreachable!("IncDec target type"),
        };
        self.store_local(target, new);
        if is_prefix { new } else { old }
    }

    fn binary(&mut self, op: BinOp, l: &mir::Expr, r: &mir::Expr) -> Val<'ctx> {
        use BinOp::*;
        let cx = self.cx;
        let lv = self.expr(l);
        let rv = self.expr(r);
        match op {
            Add => match (lv, rv) {
                (Val::Num(a), Val::Num(b)) => {
                    Val::Num(self.cx.builder.build_float_add(a, b, "add").expect("add"))
                }
                (Val::Str(_), _) | (_, Val::Str(_)) => {
                    let a = self.stringify(lv);
                    let b = self.stringify(rv);
                    let f = cx.runtime_fn(
                        "vs_string_concat",
                        Some(cx.ptr().into()),
                        &[cx.ptr().into(), cx.ptr().into()],
                    );
                    let call = self
                        .cx
                        .builder
                        .build_call(f, &[a.into(), b.into()], "concat")
                        .expect("call");
                    Val::Str(
                        call.try_as_basic_value()
                            .basic()
                            .expect("value")
                            .into_pointer_value(),
                    )
                }
                _ => {
                    // `*` involved (§11.6.1 dynamic add).
                    let a = match lv {
                        Val::Any(p) => p,
                        v => self.box_value(v),
                    };
                    let b = match rv {
                        Val::Any(p) => p,
                        v => self.box_value(v),
                    };
                    let out = self.entry_alloca(cx.any_ty, "addout");
                    let f = cx.runtime_fn(
                        "vs_any_add",
                        None,
                        &[cx.ptr().into(), cx.ptr().into(), cx.ptr().into()],
                    );
                    self.cx
                        .builder
                        .build_call(f, &[a.into(), b.into(), out.into()], "")
                        .expect("call");
                    Val::Any(out)
                }
            },
            Sub | Mul | Div | Rem => {
                let (Val::Num(a), Val::Num(b)) = (lv, rv) else {
                    unreachable!("numeric operands (sema coerced)")
                };
                let b_ = &self.cx.builder;
                Val::Num(
                    match op {
                        Sub => b_.build_float_sub(a, b, "sub"),
                        Mul => b_.build_float_mul(a, b, "mul"),
                        Div => b_.build_float_div(a, b, "div"),
                        _ => b_.build_float_rem(a, b, "rem"),
                    }
                    .expect("arith"),
                )
            }
            Shl | Shr | Ushr | BitAnd | BitOr | BitXor => {
                let (a, b) = (self.int_operand(lv), self.int_operand(rv));
                let b_ = &self.cx.builder;
                // Shift counts mask to 5 bits (§11.7.1).
                let masked = || {
                    b_.build_and(b, cx.context.i32_type().const_int(31, false), "mask")
                        .expect("and")
                };
                let result = match op {
                    Shl => b_.build_left_shift(a, masked(), "shl"),
                    Shr => b_.build_right_shift(a, masked(), true, "shr"),
                    Ushr => b_.build_right_shift(a, masked(), false, "ushr"),
                    BitAnd => b_.build_and(a, b, "and"),
                    BitOr => b_.build_or(a, b, "or"),
                    _ => b_.build_xor(a, b, "xor"),
                }
                .expect("bitop");
                if op == Ushr {
                    Val::UInt(result)
                } else {
                    Val::Int(result)
                }
            }
            Lt | Gt | Le | Ge => self.relational(op, lv, rv),
            Eq | Ne => {
                let eq = self.loose_equals(lv, rv);
                Val::Bool(if op == Ne {
                    self.cx.builder.build_not(eq, "ne").expect("not")
                } else {
                    eq
                })
            }
            StrictEq | StrictNe => {
                let eq = self.strict_equals(lv, rv);
                Val::Bool(if op == StrictNe {
                    self.cx.builder.build_not(eq, "ne").expect("not")
                } else {
                    eq
                })
            }
        }
    }

    fn int_operand(&mut self, v: Val<'ctx>) -> IntValue<'ctx> {
        match v {
            Val::Int(i) | Val::UInt(i) => i,
            _ => unreachable!("int operand (sema coerced)"),
        }
    }

    fn relational(&mut self, op: BinOp, lv: Val<'ctx>, rv: Val<'ctx>) -> Val<'ctx> {
        let cx = self.cx;
        let opnum = |op: BinOp| match op {
            BinOp::Lt => 0u64,
            BinOp::Gt => 1,
            BinOp::Le => 2,
            _ => 3,
        };
        match (lv, rv) {
            (Val::Num(a), Val::Num(b)) => {
                let pred = match op {
                    BinOp::Lt => FloatPredicate::OLT,
                    BinOp::Gt => FloatPredicate::OGT,
                    BinOp::Le => FloatPredicate::OLE,
                    _ => FloatPredicate::OGE,
                };
                Val::Bool(
                    self.cx
                        .builder
                        .build_float_compare(pred, a, b, "cmp")
                        .expect("cmp"),
                )
            }
            (Val::Str(a), Val::Str(b)) => {
                let f = cx.runtime_fn(
                    "vs_string_cmp",
                    Some(cx.context.i32_type().into()),
                    &[
                        cx.ptr().into(),
                        cx.ptr().into(),
                        cx.context.i32_type().into(),
                    ],
                );
                let o = cx.context.i32_type().const_int(opnum(op), false);
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[a.into(), b.into(), o.into()], "scmp")
                    .expect("call");
                Val::Bool(
                    self.nonzero(
                        call.try_as_basic_value()
                            .basic()
                            .expect("value")
                            .into_int_value(),
                    ),
                )
            }
            _ => {
                // Mixed / boxed comparison (§11.8.5 general case).
                let a = match lv {
                    Val::Any(p) => p,
                    v => self.box_value(v),
                };
                let b = match rv {
                    Val::Any(p) => p,
                    v => self.box_value(v),
                };
                let f = cx.runtime_fn(
                    "vs_any_cmp",
                    Some(cx.context.i32_type().into()),
                    &[
                        cx.ptr().into(),
                        cx.ptr().into(),
                        cx.context.i32_type().into(),
                    ],
                );
                let o = cx.context.i32_type().const_int(opnum(op), false);
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[a.into(), b.into(), o.into()], "acmp")
                    .expect("call");
                Val::Bool(
                    self.nonzero(
                        call.try_as_basic_value()
                            .basic()
                            .expect("value")
                            .into_int_value(),
                    ),
                )
            }
        }
    }

    /// `==` (§11.9.3): typed fast paths, boxed general case.
    fn loose_equals(&mut self, lv: Val<'ctx>, rv: Val<'ctx>) -> IntValue<'ctx> {
        self.equality(lv, rv, "vs_any_equals")
    }

    /// `===` (§11.9.6).
    fn strict_equals(&mut self, lv: Val<'ctx>, rv: Val<'ctx>) -> IntValue<'ctx> {
        self.equality(lv, rv, "vs_any_strict_equals")
    }

    fn equality(&mut self, lv: Val<'ctx>, rv: Val<'ctx>, fallback: &str) -> IntValue<'ctx> {
        let cx = self.cx;
        // Numeric pairs compare as Numbers regardless of int/uint/Number mix
        // (they are one numeric type at the language level, §11.9.6 step 5).
        if let (Some(a), Some(b)) = (self.numeric_of(lv), self.numeric_of(rv)) {
            return self
                .cx
                .builder
                .build_float_compare(FloatPredicate::OEQ, a, b, "eq")
                .expect("cmp");
        }
        match (lv, rv) {
            (Val::Bool(a), Val::Bool(b)) => self
                .cx
                .builder
                .build_int_compare(IntPredicate::EQ, a, b, "eq")
                .expect("cmp"),
            // Object reference identity (§11.9.6 step 13). A null String
            // literal compared against an object also lands here.
            (
                Val::Obj(a) | Val::Arr(a) | Val::VecP(a),
                Val::Obj(b) | Val::Arr(b) | Val::VecP(b),
            )
            | (Val::Obj(a) | Val::Arr(a) | Val::VecP(a), Val::Str(b))
            | (Val::Str(a), Val::Obj(b) | Val::Arr(b) | Val::VecP(b)) => self
                .cx
                .builder
                .build_int_compare(
                    IntPredicate::EQ,
                    self.cx
                        .builder
                        .build_ptr_to_int(a, self.cx.context.i64_type(), "pa")
                        .expect("cast"),
                    self.cx
                        .builder
                        .build_ptr_to_int(b, self.cx.context.i64_type(), "pb")
                        .expect("cast"),
                    "refeq",
                )
                .expect("cmp"),
            (Val::Str(a), Val::Str(b)) => {
                let f = cx.runtime_fn(
                    "vs_string_equals",
                    Some(cx.context.i32_type().into()),
                    &[cx.ptr().into(), cx.ptr().into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[a.into(), b.into()], "seq")
                    .expect("call");
                self.nonzero(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_int_value(),
                )
            }
            _ => {
                let a = match lv {
                    Val::Any(p) => p,
                    v => self.box_value(v),
                };
                let b = match rv {
                    Val::Any(p) => p,
                    v => self.box_value(v),
                };
                let f = cx.runtime_fn(
                    fallback,
                    Some(cx.context.i32_type().into()),
                    &[cx.ptr().into(), cx.ptr().into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[a.into(), b.into()], "aeq")
                    .expect("call");
                self.nonzero(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_int_value(),
                )
            }
        }
    }

    /// The f64 value of a statically numeric operand, if it is one.
    fn numeric_of(&mut self, v: Val<'ctx>) -> Option<FloatValue<'ctx>> {
        let cx = self.cx;
        Some(match v {
            Val::Num(f) => f,
            Val::Int(i) => self
                .cx
                .builder
                .build_signed_int_to_float(i, cx.context.f64_type(), "sitofp")
                .expect("conv"),
            Val::UInt(i) => self
                .cx
                .builder
                .build_unsigned_int_to_float(i, cx.context.f64_type(), "uitofp")
                .expect("conv"),
            _ => return None,
        })
    }

    fn logical(&mut self, is_and: bool, l: &mir::Expr, r: &mir::Expr, ty: Ty) -> Val<'ctx> {
        // Value-preserving short circuit (§11.11): materialize the lhs value
        // and its truthiness in the current block, then branch — phi
        // incomings must be computed in their predecessor blocks.
        let lhs = self.expr(l);
        let lhs_basic = self.materialize(lhs);
        let test = self.as_bool(lhs);
        let lhs_bb = self.cx.builder.get_insert_block().expect("block");
        let rhs_bb = self.new_block("logic.rhs");
        let end_bb = self.new_block("logic.end");
        // `&&` evaluates rhs when lhs is truthy; `||` when falsy.
        let (on_true, on_false) = if is_and {
            (rhs_bb, end_bb)
        } else {
            (end_bb, rhs_bb)
        };
        self.cx
            .builder
            .build_conditional_branch(test, on_true, on_false)
            .expect("br");
        self.cx.builder.position_at_end(rhs_bb);
        let rhs = self.expr(r);
        let rhs_basic = self.materialize(rhs);
        let rhs_end_bb = self.cx.builder.get_insert_block().expect("block");
        self.cx
            .builder
            .build_unconditional_branch(end_bb)
            .expect("br");
        self.cx.builder.position_at_end(end_bb);
        let phi = self
            .cx
            .builder
            .build_phi(lhs_basic.get_type(), "logic")
            .expect("phi");
        phi.add_incoming(&[(&lhs_basic, lhs_bb), (&rhs_basic, rhs_end_bb)]);
        self.wrap_basic(phi.as_basic_value(), ty)
    }

    fn conditional(&mut self, c: &mir::Expr, t: &mir::Expr, f: &mir::Expr, ty: Ty) -> Val<'ctx> {
        let cv = self.expr(c);
        let cond = self.as_bool(cv);
        let then_bb = self.new_block("cond.then");
        let else_bb = self.new_block("cond.else");
        let end_bb = self.new_block("cond.end");
        self.cx
            .builder
            .build_conditional_branch(cond, then_bb, else_bb)
            .expect("br");
        self.cx.builder.position_at_end(then_bb);
        let tv = self.expr(t);
        let t_basic = self.materialize(tv);
        let t_end = self.cx.builder.get_insert_block().expect("block");
        self.cx
            .builder
            .build_unconditional_branch(end_bb)
            .expect("br");
        self.cx.builder.position_at_end(else_bb);
        let fv = self.expr(f);
        let f_basic = self.materialize(fv);
        let f_end = self.cx.builder.get_insert_block().expect("block");
        self.cx
            .builder
            .build_unconditional_branch(end_bb)
            .expect("br");
        self.cx.builder.position_at_end(end_bb);
        if ty == Ty::Void {
            return Val::Void;
        }
        let phi = self
            .cx
            .builder
            .build_phi(t_basic.get_type(), "cond")
            .expect("phi");
        phi.add_incoming(&[(&t_basic, t_end), (&f_basic, f_end)]);
        self.wrap_basic(phi.as_basic_value(), ty)
    }

    // --- calls -----------------------------------------------------------------

    fn builtin(&mut self, b: Builtin, args: &[mir::Expr]) -> Val<'ctx> {
        let cx = self.cx;
        match b {
            Builtin::Trace => {
                // Stage the boxed args into a stack array and hand it to the
                // runtime (ABI: vs_trace(argc, *const VsAny)).
                let argc = args.len() as u32;
                let arr_ty = cx.any_ty.array_type(argc.max(1));
                let arr = self.entry_alloca(arr_ty, "traceargs");
                for (i, a) in args.iter().enumerate() {
                    let p = self.as_any_ptr(a);
                    let v = self
                        .cx
                        .builder
                        .build_load(cx.any_ty, p, "arg")
                        .expect("load");
                    let slot = unsafe {
                        self.cx.builder.build_in_bounds_gep(
                            arr_ty,
                            arr,
                            &[
                                cx.context.i32_type().const_zero(),
                                cx.context.i32_type().const_int(i as u64, false),
                            ],
                            "slot",
                        )
                    }
                    .expect("gep");
                    self.cx.builder.build_store(slot, v).expect("store");
                }
                let f = cx.runtime_fn(
                    "vs_trace",
                    None,
                    &[cx.context.i32_type().into(), cx.ptr().into()],
                );
                let n = cx.context.i32_type().const_int(u64::from(argc), false);
                self.cx
                    .builder
                    .build_call(f, &[n.into(), arr.into()], "")
                    .expect("call");
                Val::Void
            }
            Builtin::ParseInt => {
                let s = self.str_arg(&args[0]);
                let radix = match self.expr(&args[1]) {
                    Val::Int(i) | Val::UInt(i) => i,
                    _ => unreachable!("radix type"),
                };
                let f = cx.runtime_fn(
                    "vs_parse_int",
                    Some(cx.context.f64_type().into()),
                    &[cx.ptr().into(), cx.context.i32_type().into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[s.into(), radix.into()], "parseint")
                    .expect("call");
                Val::Num(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_float_value(),
                )
            }
            Builtin::ParseFloat => {
                let s = self.str_arg(&args[0]);
                let f = cx.runtime_fn(
                    "vs_parse_float",
                    Some(cx.context.f64_type().into()),
                    &[cx.ptr().into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[s.into()], "parsefloat")
                    .expect("call");
                Val::Num(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_float_value(),
                )
            }
            Builtin::EncodeUriComponent
            | Builtin::DecodeUriComponent
            | Builtin::Escape
            | Builtin::Unescape => {
                let name = match b {
                    Builtin::EncodeUriComponent => "vs_encode_uri_component",
                    Builtin::DecodeUriComponent => "vs_decode_uri_component",
                    Builtin::Escape => "vs_escape",
                    _ => "vs_unescape",
                };
                let s = self.str_arg(&args[0]);
                let f = cx.runtime_fn(name, Some(cx.ptr().into()), &[cx.ptr().into()]);
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[s.into()], "uri")
                    .expect("call");
                Val::Str(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            Builtin::IsNaN => {
                let v = self.num_arg(&args[0]);
                // NaN is the only value unordered with itself.
                Val::Bool(
                    self.cx
                        .builder
                        .build_float_compare(FloatPredicate::UNO, v, v, "isnan")
                        .expect("cmp"),
                )
            }
            Builtin::IsFinite => {
                let v = self.num_arg(&args[0]);
                let sub = self
                    .cx
                    .builder
                    .build_float_sub(v, v, "self_sub")
                    .expect("sub");
                // x - x == 0 only for finite x (NaN/Inf give NaN).
                Val::Bool(
                    self.cx
                        .builder
                        .build_float_compare(
                            FloatPredicate::OEQ,
                            sub,
                            cx.context.f64_type().const_zero(),
                            "isfinite",
                        )
                        .expect("cmp"),
                )
            }
        }
    }

    fn str_arg(&mut self, e: &mir::Expr) -> PointerValue<'ctx> {
        match self.expr(e) {
            Val::Str(p) => p,
            v => self.stringify(v),
        }
    }

    fn num_arg(&mut self, e: &mir::Expr) -> FloatValue<'ctx> {
        let v = self.expr(e);
        self.numeric_of(v).unwrap_or_else(|| match v {
            Val::Any(p) => self.any_to_number(p),
            _ => unreachable!("numeric argument"),
        })
    }

    fn str_method(&mut self, m: StrMethod, recv: &mir::Expr, args: &[mir::Expr]) -> Val<'ctx> {
        let cx = self.cx;
        let this = self.str_arg(recv);
        if m == StrMethod::Replace {
            let this = self.str_arg(recv);
            let search = self.str_arg(&args[0]);
            let repl = self.str_arg(&args[1]);
            let f = cx.runtime_fn(
                "vs_str_replace",
                Some(cx.ptr().into()),
                &[cx.ptr().into(), cx.ptr().into(), cx.ptr().into()],
            );
            let call = self
                .cx
                .builder
                .build_call(f, &[this.into(), search.into(), repl.into()], "repl")
                .expect("call");
            return Val::Str(
                call.try_as_basic_value()
                    .basic()
                    .expect("value")
                    .into_pointer_value(),
            );
        }
        if m == StrMethod::Split {
            let this = self.str_arg(recv);
            let delim = self.str_arg(&args[0]);
            let limit = self.num_arg(&args[1]);
            let f = cx.runtime_fn(
                "vs_str_split",
                Some(cx.ptr().into()),
                &[
                    cx.ptr().into(),
                    cx.ptr().into(),
                    cx.context.f64_type().into(),
                ],
            );
            let call = self
                .cx
                .builder
                .build_call(f, &[this.into(), delim.into(), limit.into()], "split")
                .expect("call");
            return Val::Arr(
                call.try_as_basic_value()
                    .basic()
                    .expect("value")
                    .into_pointer_value(),
            );
        }
        let (name, arg_kinds, ret_str): (&str, &[ArgKind], bool) = match m {
            StrMethod::Split | StrMethod::Replace => unreachable!("handled above"),
            StrMethod::CharAt => ("vs_str_char_at", &[ArgKind::Num], true),
            StrMethod::CharCodeAt => ("vs_str_char_code_at", &[ArgKind::Num], false),
            StrMethod::IndexOf => ("vs_str_index_of", &[ArgKind::Str, ArgKind::Num], false),
            StrMethod::LastIndexOf => {
                ("vs_str_last_index_of", &[ArgKind::Str, ArgKind::Num], false)
            }
            StrMethod::Slice => ("vs_str_slice", &[ArgKind::Num, ArgKind::Num], true),
            StrMethod::Substring => ("vs_str_substring", &[ArgKind::Num, ArgKind::Num], true),
            StrMethod::Substr => ("vs_str_substr", &[ArgKind::Num, ArgKind::Num], true),
            StrMethod::ToLowerCase => ("vs_str_to_lower", &[], true),
            StrMethod::ToUpperCase => ("vs_str_to_upper", &[], true),
            StrMethod::ToString => ("vs_str_to_string", &[], true),
        };
        let mut param_tys: Vec<BasicMetadataTypeEnum> = vec![cx.ptr().into()];
        let mut argv: Vec<BasicMetadataValueEnum> = vec![this.into()];
        for (kind, a) in arg_kinds.iter().zip(args) {
            match kind {
                ArgKind::Num => {
                    param_tys.push(cx.context.f64_type().into());
                    argv.push(self.num_arg(a).into());
                }
                ArgKind::Str => {
                    param_tys.push(cx.ptr().into());
                    argv.push(self.str_arg(a).into());
                }
            }
        }
        let ret: BasicTypeEnum = if ret_str {
            cx.ptr().into()
        } else if m == StrMethod::CharCodeAt {
            cx.context.f64_type().into()
        } else {
            cx.context.i32_type().into()
        };
        let f = cx.runtime_fn(name, Some(ret), &param_tys);
        let call = self.cx.builder.build_call(f, &argv, "strm").expect("call");
        let out = call.try_as_basic_value().basic().expect("value");
        if ret_str {
            Val::Str(out.into_pointer_value())
        } else if m == StrMethod::CharCodeAt {
            Val::Num(out.into_float_value())
        } else {
            Val::Int(out.into_int_value())
        }
    }

    fn num_method(&mut self, m: NumMethod, recv: &mir::Expr, args: &[mir::Expr]) -> Val<'ctx> {
        let cx = self.cx;
        let this = self.num_arg(recv);
        let arg = self.num_arg(&args[0]);
        let name = match m {
            NumMethod::ToString => "vs_num_to_string_radix",
            NumMethod::ToFixed => "vs_num_to_fixed",
        };
        let f = cx.runtime_fn(
            name,
            Some(cx.ptr().into()),
            &[cx.context.f64_type().into(), cx.context.f64_type().into()],
        );
        let call = self
            .cx
            .builder
            .build_call(f, &[this.into(), arg.into()], "numm")
            .expect("call");
        Val::Str(
            call.try_as_basic_value()
                .basic()
                .expect("value")
                .into_pointer_value(),
        )
    }
}

impl<'a, 'ctx> FnCx<'a, 'ctx> {
    /// Stages expressions as a stack array of boxed values; holes become
    /// undefined. Returns the array pointer.
    fn stage_any_array_opt(&mut self, elements: &[Option<&mir::Expr>]) -> PointerValue<'ctx> {
        let cx = self.cx;
        let n = elements.len() as u32;
        let arr_ty = cx.any_ty.array_type(n.max(1));
        let arr = self.entry_alloca(arr_ty, "staged");
        for (i, el) in elements.iter().enumerate() {
            let v = match el {
                Some(e) => {
                    let val = self.expr(e);
                    let p = match val {
                        Val::Any(p) => p,
                        other => self.box_value(other),
                    };
                    self.cx
                        .builder
                        .build_load(cx.any_ty, p, "el")
                        .expect("load")
                }
                None => cx.any_ty.const_zero().into(),
            };
            let slot = unsafe {
                self.cx.builder.build_in_bounds_gep(
                    arr_ty,
                    arr,
                    &[
                        cx.context.i32_type().const_zero(),
                        cx.context.i32_type().const_int(i as u64, false),
                    ],
                    "slot",
                )
            }
            .expect("gep");
            self.cx.builder.build_store(slot, v).expect("store");
        }
        arr
    }

    /// Sequence receiver: pointer + is-vector flag.
    fn seq_ptr(&mut self, recv: &mir::Expr) -> (PointerValue<'ctx>, bool) {
        let is_vec = matches!(recv.ty, Ty::Vector(_));
        let v = self.expr(recv);
        match v {
            Val::Arr(p) | Val::VecP(p) | Val::Str(p) | Val::Obj(p) => (p, is_vec),
            _ => unreachable!("sequence receiver"),
        }
    }

    /// Storage kind of a `Vector.<T>` instantiation (P23; must agree with
    /// `runtime::seq::VEC_*`). Only the numeric kinds are stored unboxed
    /// and get inlined element access; every other element type is boxed.
    fn vec_kind(&self, inst: u32) -> u32 {
        match self.program.vectors[inst as usize] {
            Ty::Number => 1, // VEC_F64
            Ty::Int => 2,    // VEC_I32
            Ty::UInt => 3,   // VEC_U32
            _ => 0,          // VEC_BOXED
        }
    }

    /// If `recv` is a `Vector` with an unboxed numeric element kind,
    /// returns `(kind, llvm_elem_ty, stride_bytes)`; otherwise `None`.
    fn unboxed_vec(&self, recv: &mir::Expr) -> Option<(u32, BasicTypeEnum<'ctx>, u64)> {
        let Ty::Vector(inst) = recv.ty else {
            return None;
        };
        let cx = self.cx;
        match self.vec_kind(inst) {
            1 => Some((1, cx.context.f64_type().into(), 8)),
            2 => Some((2, cx.context.i32_type().into(), 4)),
            3 => Some((3, cx.context.i32_type().into(), 4)),
            _ => None,
        }
    }

    /// Loads `(len, data_ptr)` from a flat `VsVector` header (P23 layout:
    /// `len` is the third `i32`, byte 8; `data` is the pointer at byte 16).
    fn vec_header(&self, p: PointerValue<'ctx>) -> (IntValue<'ctx>, PointerValue<'ctx>) {
        let cx = self.cx;
        let i32t = cx.context.i32_type();
        let i8t = cx.context.i8_type();
        let len_ptr = unsafe {
            cx.builder
                .build_in_bounds_gep(i32t, p, &[i32t.const_int(2, false)], "vlen_p")
                .expect("gep")
        };
        let len = cx
            .builder
            .build_load(i32t, len_ptr, "vlen")
            .expect("load")
            .into_int_value();
        let data_pp = unsafe {
            cx.builder
                .build_in_bounds_gep(i8t, p, &[i8t.const_int(16, false)], "vdata_pp")
                .expect("gep")
        };
        let data = cx
            .builder
            .build_load(cx.ptr(), data_pp, "vdata")
            .expect("load")
            .into_pointer_value();
        (len, data)
    }

    /// Coerces a store operand to an unboxed Vector element's storage type
    /// so the LLVM store is type-exact (sema already coerced to the element
    /// AS3 type; this just selects the matching scalar — `kind`: 1=f64,
    /// 2=i32, 3=u32).
    fn vec_scalar(&mut self, value: Val<'ctx>, kind: u32) -> BasicValueEnum<'ctx> {
        if kind == 1 {
            self.numeric_of(value).expect("numeric").into()
        } else {
            match value {
                Val::Int(i) | Val::UInt(i) => i.into(),
                other => {
                    let f = self.numeric_of(other).expect("numeric");
                    let conv = if kind == 2 { Conv::ToInt } else { Conv::ToUInt };
                    match self.convert_num_to_int(f, conv) {
                        Val::Int(i) | Val::UInt(i) => i.into(),
                        _ => unreachable!("int conversion"),
                    }
                }
            }
        }
    }

    /// `0.0 <= idx < len` as an `i1` (both bounds; `idx` is the raw f64 index).
    fn idx_in_bounds(&self, idx_f: FloatValue<'ctx>, len_f: FloatValue<'ctx>) -> IntValue<'ctx> {
        let b = &self.cx.builder;
        let zero = self.cx.context.f64_type().const_float(0.0);
        let ge0 = b
            .build_float_compare(inkwell::FloatPredicate::OGE, idx_f, zero, "ge0")
            .expect("cmp");
        let lt = b
            .build_float_compare(inkwell::FloatPredicate::OLT, idx_f, len_f, "lt")
            .expect("cmp");
        b.build_and(ge0, lt, "inb").expect("and")
    }

    /// Unboxes a `VsAny` alloca into a typed value (ES3 §9 conversions —
    /// the uniform-storage boundary of SPECS §4.2).
    fn unbox_any_ptr(&mut self, p: PointerValue<'ctx>, ty: Ty) -> Val<'ctx> {
        let cx = self.cx;
        match ty {
            Ty::Any => Val::Any(p),
            Ty::Int | Ty::UInt => {
                let n = self.any_to_number(p);
                self.convert_num_to_int(
                    n,
                    if ty == Ty::Int {
                        Conv::ToInt
                    } else {
                        Conv::ToUInt
                    },
                )
            }
            Ty::Number => Val::Num(self.any_to_number(p)),
            Ty::Boolean => {
                let f = cx.runtime_fn(
                    "vs_any_truthy",
                    Some(cx.context.i32_type().into()),
                    &[cx.ptr().into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[p.into()], "b")
                    .expect("call");
                Val::Bool(
                    self.nonzero(
                        call.try_as_basic_value()
                            .basic()
                            .expect("value")
                            .into_int_value(),
                    ),
                )
            }
            Ty::String => {
                let f = cx.runtime_fn(
                    "vs_any_coerce_string",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[p.into()], "s")
                    .expect("call");
                Val::Str(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            Ty::Object(class) => {
                let f = cx.runtime_fn(
                    "vs_any_coerce_class",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into(), cx.ptr().into()],
                );
                let rtti = cx.classes[class as usize].rtti;
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[p.into(), rtti.into()], "o")
                    .expect("call");
                Val::Obj(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            Ty::Iface(iface) => {
                let f = cx.runtime_fn(
                    "vs_any_coerce_iface",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into(), cx.context.i32_type().into()],
                );
                let id = cx.context.i32_type().const_int(u64::from(iface), false);
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[p.into(), id.into()], "i")
                    .expect("call");
                Val::Obj(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            Ty::Array => {
                let f = cx.runtime_fn(
                    "vs_any_coerce_array",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[p.into()], "a")
                    .expect("call");
                Val::Arr(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            Ty::Vector(inst) => {
                let f = cx.runtime_fn(
                    "vs_any_coerce_vector",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into(), cx.context.i32_type().into()],
                );
                let id = cx.context.i32_type().const_int(u64::from(inst), false);
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[p.into(), id.into()], "v")
                    .expect("call");
                Val::VecP(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            Ty::Namespace => {
                // Sema gates `*` → Namespace coercions; only `as` reaches
                // here through tag matching.
                let f = cx.runtime_fn(
                    "vs_any_as_ptr",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into(), cx.context.i32_type().into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(
                        f,
                        &[p.into(), cx.context.i32_type().const_int(14, false).into()],
                        "nsv",
                    )
                    .expect("call");
                Val::Ns(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            Ty::Socket => {
                let f = cx.runtime_fn(
                    "vs_any_to_socket",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[p.into()], "skv")
                    .expect("call");
                Val::Sock(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            Ty::Date => {
                let f = cx.runtime_fn("vs_any_to_date", Some(cx.ptr().into()), &[cx.ptr().into()]);
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[p.into()], "dv")
                    .expect("call");
                Val::Dat(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            Ty::RegExp => {
                let f = cx.runtime_fn(
                    "vs_any_to_regexp",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[p.into()], "rev")
                    .expect("call");
                Val::Reg(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            Ty::Function => {
                let f = cx.runtime_fn(
                    "vs_any_coerce_function",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[p.into()], "fv")
                    .expect("call");
                Val::Fun(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            Ty::Void => Val::Void,
        }
    }

    /// RegExp/String regex operations (mir::RegexOp; runtime regexp.rs).
    fn call_regex(&mut self, op: mir::RegexOp, operands: &[mir::Expr]) -> Val<'ctx> {
        use mir::RegexOp as R;
        let cx = self.cx;
        let i32t = cx.context.i32_type();
        let ptr_of = |slf: &mut Self, e: &mir::Expr| -> PointerValue<'ctx> {
            match slf.expr(e) {
                Val::Reg(p) | Val::Str(p) => p,
                _ => unreachable!("regex operand shape"),
            }
        };
        // Flag bits mirror runtime::regexp::flag.
        let flag_mask = |op: R| match op {
            R::Global => 1u64,
            R::IgnoreCase => 2,
            R::Multiline => 4,
            _ => unreachable!(),
        };
        match op {
            R::Test => {
                let re = ptr_of(self, &operands[0]);
                let s = self.str_arg(&operands[1]);
                let f = cx.runtime_fn(
                    "vs_regexp_test",
                    Some(i32t.into()),
                    &[cx.ptr().into(), cx.ptr().into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[re.into(), s.into()], "retest")
                    .expect("call");
                let raw = call
                    .try_as_basic_value()
                    .basic()
                    .expect("value")
                    .into_int_value();
                Val::Bool(
                    self.cx
                        .builder
                        .build_int_truncate(raw, cx.context.bool_type(), "b")
                        .expect("trunc"),
                )
            }
            R::Exec | R::Match => {
                let (re, s) = if op == R::Exec {
                    (ptr_of(self, &operands[0]), self.str_arg(&operands[1]))
                } else {
                    (ptr_of(self, &operands[1]), self.str_arg(&operands[0]))
                };
                let name = if op == R::Exec {
                    "vs_regexp_exec"
                } else {
                    "vs_str_match_re"
                };
                let f = cx.runtime_fn(
                    name,
                    Some(cx.ptr().into()),
                    &[cx.ptr().into(), cx.ptr().into()],
                );
                let args: [inkwell::values::BasicMetadataValueEnum; 2] = if op == R::Exec {
                    [re.into(), s.into()]
                } else {
                    [s.into(), re.into()]
                };
                let call = self.cx.builder.build_call(f, &args, "rearr").expect("call");
                Val::Arr(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            R::Search => {
                let s = self.str_arg(&operands[0]);
                let re = ptr_of(self, &operands[1]);
                let f = cx.runtime_fn(
                    "vs_str_search_re",
                    Some(i32t.into()),
                    &[cx.ptr().into(), cx.ptr().into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[s.into(), re.into()], "research")
                    .expect("call");
                Val::Int(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_int_value(),
                )
            }
            R::Replace => {
                let s = self.str_arg(&operands[0]);
                let re = ptr_of(self, &operands[1]);
                let repl = self.str_arg(&operands[2]);
                let f = cx.runtime_fn(
                    "vs_str_replace_re",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into(), cx.ptr().into(), cx.ptr().into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[s.into(), re.into(), repl.into()], "rerepl")
                    .expect("call");
                Val::Str(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            R::ToString | R::Source => {
                let re = ptr_of(self, &operands[0]);
                let name = if op == R::ToString {
                    "vs_regexp_display"
                } else {
                    "vs_regexp_source"
                };
                let f = cx.runtime_fn(name, Some(cx.ptr().into()), &[cx.ptr().into()]);
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[re.into()], "restr")
                    .expect("call");
                Val::Str(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            R::Global | R::IgnoreCase | R::Multiline => {
                let re = ptr_of(self, &operands[0]);
                let f = cx.runtime_fn(
                    "vs_regexp_flag",
                    Some(i32t.into()),
                    &[cx.ptr().into(), i32t.into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(
                        f,
                        &[re.into(), i32t.const_int(flag_mask(op), false).into()],
                        "reflag",
                    )
                    .expect("call");
                let raw = call
                    .try_as_basic_value()
                    .basic()
                    .expect("value")
                    .into_int_value();
                Val::Bool(
                    self.cx
                        .builder
                        .build_int_truncate(raw, cx.context.bool_type(), "b")
                        .expect("trunc"),
                )
            }
            R::LastIndex => {
                let re = ptr_of(self, &operands[0]);
                let f = cx.runtime_fn(
                    "vs_regexp_last_index",
                    Some(i32t.into()),
                    &[cx.ptr().into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[re.into()], "relast")
                    .expect("call");
                Val::Int(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_int_value(),
                )
            }
        }
    }

    /// Stages Number args into an f64 stack array; returns its address.
    fn stage_f64_args(&mut self, args: &[mir::Expr], name: &str) -> PointerValue<'ctx> {
        let cx = self.cx;
        let f64t = cx.context.f64_type();
        let i32t = cx.context.i32_type();
        let arr_ty = f64t.array_type((args.len() as u32).max(1));
        let arr = self.entry_alloca(arr_ty, name);
        for (i, a) in args.iter().enumerate() {
            let v = match self.expr(a) {
                Val::Num(x) => x,
                _ => unreachable!("Number argument"),
            };
            let slot = unsafe {
                self.cx.builder.build_in_bounds_gep(
                    arr_ty,
                    arr,
                    &[i32t.const_zero(), i32t.const_int(i as u64, false)],
                    "part",
                )
            }
            .expect("gep");
            self.cx.builder.build_store(slot, v).expect("store");
        }
        arr
    }

    /// Socket instance ops (mir::SocketOp; runtime socket.rs).
    fn call_socket(&mut self, op: mir::SocketOp, operands: &[mir::Expr]) -> Val<'ctx> {
        use mir::SocketOp as O;
        let cx = self.cx;
        let i32t = cx.context.i32_type();
        let recv = match self.expr(&operands[0]) {
            Val::Sock(p) => p,
            _ => unreachable!("Socket receiver"),
        };
        match op {
            O::Write => {
                let data = self.str_arg(&operands[1]);
                let f = cx.runtime_fn("vs_socket_write", None, &[cx.ptr().into(), cx.ptr().into()]);
                self.cx
                    .builder
                    .build_call(f, &[recv.into(), data.into()], "")
                    .expect("call");
                Val::Void
            }
            O::ReadLine => {
                let f = cx.runtime_fn(
                    "vs_socket_read_line",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[recv.into()], "line")
                    .expect("call");
                Val::Str(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            O::Read => {
                let max = match self.expr(&operands[1]) {
                    Val::Int(i) | Val::UInt(i) => i,
                    _ => unreachable!("read max"),
                };
                let f = cx.runtime_fn(
                    "vs_socket_read",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into(), i32t.into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[recv.into(), max.into()], "chunk")
                    .expect("call");
                Val::Str(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            O::Close => {
                let f = cx.runtime_fn("vs_socket_close", None, &[cx.ptr().into()]);
                self.cx
                    .builder
                    .build_call(f, &[recv.into()], "")
                    .expect("call");
                Val::Void
            }
            O::Accept => {
                let f = cx.runtime_fn(
                    "vs_socket_accept",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[recv.into()], "client")
                    .expect("call");
                Val::Sock(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            O::LocalPort => {
                let f = cx.runtime_fn(
                    "vs_socket_local_port",
                    Some(i32t.into()),
                    &[cx.ptr().into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[recv.into()], "port")
                    .expect("call");
                Val::Int(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_int_value(),
                )
            }
        }
    }

    /// Date instance ops (mir::DateFn; runtime date.rs).
    fn call_date(&mut self, f: mir::DateFn, operands: &[mir::Expr]) -> Val<'ctx> {
        let cx = self.cx;
        let i32t = cx.context.i32_type();
        let f64t = cx.context.f64_type();
        let recv = match self.expr(&operands[0]) {
            Val::Dat(p) => p,
            _ => unreachable!("Date receiver"),
        };
        match f {
            mir::DateFn::Get(index) => {
                let rf = cx.runtime_fn(
                    "vs_date_get",
                    Some(f64t.into()),
                    &[cx.ptr().into(), i32t.into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(
                        rf,
                        &[recv.into(), i32t.const_int(u64::from(index), false).into()],
                        "dget",
                    )
                    .expect("call");
                Val::Num(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_float_value(),
                )
            }
            mir::DateFn::SetTime => {
                let v = match self.expr(&operands[1]) {
                    Val::Num(x) => x,
                    _ => unreachable!("setTime arg"),
                };
                let rf = cx.runtime_fn(
                    "vs_date_set_time",
                    Some(f64t.into()),
                    &[cx.ptr().into(), f64t.into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(rf, &[recv.into(), v.into()], "dset")
                    .expect("call");
                Val::Num(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_float_value(),
                )
            }
            mir::DateFn::Format(index) => {
                let rf = cx.runtime_fn(
                    "vs_date_to_string",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into(), i32t.into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(
                        rf,
                        &[recv.into(), i32t.const_int(u64::from(index), false).into()],
                        "dstr",
                    )
                    .expect("call");
                Val::Str(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
        }
    }

    fn call_arr(&mut self, m: mir::ArrMethod, recv: &mir::Expr, args: &[mir::Expr]) -> Val<'ctx> {
        use mir::ArrMethod::*;
        let cx = self.cx;
        let (p, _) = self.seq_ptr(recv);
        let i32t = cx.context.i32_type();
        let f64t = cx.context.f64_type();
        match m {
            Push | Unshift => {
                let staged: Vec<Option<&mir::Expr>> = args.iter().map(Some).collect();
                let buf = self.stage_any_array_opt(&staged);
                let name = if m == Push {
                    "vs_arr_push"
                } else {
                    "vs_arr_unshift"
                };
                let f = cx.runtime_fn(
                    name,
                    Some(i32t.into()),
                    &[cx.ptr().into(), i32t.into(), cx.ptr().into()],
                );
                let n = i32t.const_int(args.len() as u64, false);
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[p.into(), n.into(), buf.into()], "n")
                    .expect("call");
                Val::UInt(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_int_value(),
                )
            }
            Pop | Shift => {
                let out = self.entry_alloca(cx.any_ty, "popped");
                let name = if m == Pop {
                    "vs_arr_pop"
                } else {
                    "vs_arr_shift"
                };
                let f = cx.runtime_fn(name, None, &[cx.ptr().into(), cx.ptr().into()]);
                self.cx
                    .builder
                    .build_call(f, &[p.into(), out.into()], "")
                    .expect("call");
                Val::Any(out)
            }
            Slice => {
                let a = self.num_arg_or(args, 0, 0.0);
                let b = self.num_arg_or(args, 1, f64::MAX);
                let f = cx.runtime_fn(
                    "vs_arr_slice",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into(), f64t.into(), f64t.into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[p.into(), a.into(), b.into()], "sl")
                    .expect("call");
                Val::Arr(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            Splice => {
                let a = self.num_arg_or(args, 0, 0.0);
                let b = self.num_arg_or(args, 1, f64::MAX);
                let rest = if args.len() > 2 { &args[2..] } else { &[] };
                let staged: Vec<Option<&mir::Expr>> = rest.iter().map(Some).collect();
                let buf = self.stage_any_array_opt(&staged);
                let f = cx.runtime_fn(
                    "vs_arr_splice",
                    Some(cx.ptr().into()),
                    &[
                        cx.ptr().into(),
                        f64t.into(),
                        f64t.into(),
                        i32t.into(),
                        cx.ptr().into(),
                    ],
                );
                let n = i32t.const_int(rest.len() as u64, false);
                let call = self
                    .cx
                    .builder
                    .build_call(
                        f,
                        &[p.into(), a.into(), b.into(), n.into(), buf.into()],
                        "sp",
                    )
                    .expect("call");
                Val::Arr(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            IndexOf => {
                let needle = self.expr(&args[0]);
                let np = match needle {
                    Val::Any(p) => p,
                    other => self.box_value(other),
                };
                let from = self.num_arg_or(args, 1, 0.0);
                let f = cx.runtime_fn(
                    "vs_arr_index_of",
                    Some(i32t.into()),
                    &[cx.ptr().into(), cx.ptr().into(), f64t.into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[p.into(), np.into(), from.into()], "idx")
                    .expect("call");
                Val::Int(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_int_value(),
                )
            }
            Concat => {
                let staged: Vec<Option<&mir::Expr>> = args.iter().map(Some).collect();
                let buf = self.stage_any_array_opt(&staged);
                let f = cx.runtime_fn(
                    "vs_arr_concat",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into(), i32t.into(), cx.ptr().into()],
                );
                let n = i32t.const_int(args.len() as u64, false);
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[p.into(), n.into(), buf.into()], "cc")
                    .expect("call");
                Val::Arr(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            Join => {
                let sep = if args.is_empty() {
                    cx.ptr().const_null()
                } else {
                    self.str_arg(&args[0])
                };
                let f = cx.runtime_fn(
                    "vs_arr_join",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into(), cx.ptr().into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[p.into(), sep.into()], "j")
                    .expect("call");
                Val::Str(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            ForEach | Map | Filter | SomeM | Every => {
                let cb = match self.expr(&args[0]) {
                    Val::Fun(c) => c,
                    Val::Any(pp) => {
                        let f = cx.runtime_fn(
                            "vs_any_coerce_function",
                            Some(cx.ptr().into()),
                            &[cx.ptr().into()],
                        );
                        self.cx
                            .builder
                            .build_call(f, &[pp.into()], "cb")
                            .expect("call")
                            .try_as_basic_value()
                            .basic()
                            .expect("value")
                            .into_pointer_value()
                    }
                    _ => unreachable!("callback type"),
                };
                let mode = i32t.const_int(
                    match m {
                        ForEach => 0,
                        Map => 1,
                        Filter => 2,
                        SomeM => 3,
                        _ => 4,
                    },
                    false,
                );
                let out = self.entry_alloca(cx.any_ty, "iter");
                let f = cx.runtime_fn(
                    "vs_arr_iterate",
                    None,
                    &[
                        cx.ptr().into(),
                        cx.ptr().into(),
                        i32t.into(),
                        cx.ptr().into(),
                    ],
                );
                self.cx
                    .builder
                    .build_call(f, &[p.into(), cb.into(), mode.into(), out.into()], "")
                    .expect("call");
                match m {
                    ForEach => Val::Void,
                    Map | Filter => self.unbox_any_ptr(out, Ty::Array),
                    _ => {
                        let f =
                            cx.runtime_fn("vs_any_truthy", Some(i32t.into()), &[cx.ptr().into()]);
                        let call = self
                            .cx
                            .builder
                            .build_call(f, &[out.into()], "b")
                            .expect("call");
                        Val::Bool(
                            self.nonzero(
                                call.try_as_basic_value()
                                    .basic()
                                    .expect("value")
                                    .into_int_value(),
                            ),
                        )
                    }
                }
            }
            Sort if !args.is_empty() => {
                let cmp = match self.expr(&args[0]) {
                    Val::Fun(c) => c,
                    Val::Any(pp) => {
                        let f = cx.runtime_fn(
                            "vs_any_coerce_function",
                            Some(cx.ptr().into()),
                            &[cx.ptr().into()],
                        );
                        self.cx
                            .builder
                            .build_call(f, &[pp.into()], "cmp")
                            .expect("call")
                            .try_as_basic_value()
                            .basic()
                            .expect("value")
                            .into_pointer_value()
                    }
                    _ => unreachable!("comparator type"),
                };
                let f = cx.runtime_fn(
                    "vs_arr_sort_with",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into(), cx.ptr().into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[p.into(), cmp.into()], "sw")
                    .expect("call");
                Val::Arr(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            Reverse | Sort => {
                let name = if m == Reverse {
                    "vs_arr_reverse"
                } else {
                    "vs_arr_sort"
                };
                let f = cx.runtime_fn(name, Some(cx.ptr().into()), &[cx.ptr().into()]);
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[p.into()], "r")
                    .expect("call");
                Val::Arr(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
        }
    }

    fn call_vec(
        &mut self,
        m: mir::VecMethod,
        recv: &mir::Expr,
        args: &[mir::Expr],
        expr_ty: Ty,
    ) -> Val<'ctx> {
        use mir::VecMethod::*;
        let cx = self.cx;
        let (p, _) = self.seq_ptr(recv);
        let i32t = cx.context.i32_type();
        let f64t = cx.context.f64_type();
        match m {
            Push | Unshift => {
                let staged: Vec<Option<&mir::Expr>> = args.iter().map(Some).collect();
                let buf = self.stage_any_array_opt(&staged);
                let name = if m == Push {
                    "vs_vec_push"
                } else {
                    "vs_vec_unshift"
                };
                let f = cx.runtime_fn(
                    name,
                    Some(i32t.into()),
                    &[cx.ptr().into(), i32t.into(), cx.ptr().into()],
                );
                let n = i32t.const_int(args.len() as u64, false);
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[p.into(), n.into(), buf.into()], "n")
                    .expect("call");
                Val::UInt(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_int_value(),
                )
            }
            Pop | Shift => {
                let out = self.entry_alloca(cx.any_ty, "popped");
                let name = if m == Pop {
                    "vs_vec_pop"
                } else {
                    "vs_vec_shift"
                };
                let f = cx.runtime_fn(name, None, &[cx.ptr().into(), cx.ptr().into()]);
                self.cx
                    .builder
                    .build_call(f, &[p.into(), out.into()], "")
                    .expect("call");
                self.unbox_any_ptr(out, expr_ty)
            }
            Slice => {
                let a = self.num_arg_or(args, 0, 0.0);
                let b = self.num_arg_or(args, 1, f64::MAX);
                let f = cx.runtime_fn(
                    "vs_vec_slice",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into(), f64t.into(), f64t.into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[p.into(), a.into(), b.into()], "sl")
                    .expect("call");
                Val::VecP(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            IndexOf => {
                let needle = self.expr(&args[0]);
                let np = match needle {
                    Val::Any(p) => p,
                    other => self.box_value(other),
                };
                let from = self.num_arg_or(args, 1, 0.0);
                let f = cx.runtime_fn(
                    "vs_vec_index_of",
                    Some(i32t.into()),
                    &[cx.ptr().into(), cx.ptr().into(), f64t.into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[p.into(), np.into(), from.into()], "idx")
                    .expect("call");
                Val::Int(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_int_value(),
                )
            }
            Join => {
                let sep = if args.is_empty() {
                    cx.ptr().const_null()
                } else {
                    self.str_arg(&args[0])
                };
                let f = cx.runtime_fn(
                    "vs_vec_join",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into(), cx.ptr().into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[p.into(), sep.into()], "j")
                    .expect("call");
                Val::Str(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            Reverse => {
                let f = cx.runtime_fn("vs_vec_reverse", Some(cx.ptr().into()), &[cx.ptr().into()]);
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[p.into()], "r")
                    .expect("call");
                Val::VecP(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
        }
    }

    /// f64 argument at `i`, or the default when omitted.
    fn num_arg_or(&mut self, args: &[mir::Expr], i: usize, default: f64) -> FloatValue<'ctx> {
        match args.get(i) {
            Some(a) => self.num_arg(a),
            None => self.cx.context.f64_type().const_float(default),
        }
    }
}

/// Builds RTTI globals, instance layouts, interface tables, and static
/// storage for every class (layout contract: runtime/src/object.rs).
fn build_class_artifacts<'ctx>(
    cx: &Cx<'ctx>,
    program: &mir::Program,
    fns: &[FunctionValue<'ctx>],
    td: &inkwell::targets::TargetData,
) -> Vec<ClassArt<'ctx>> {
    let i32t = cx.context.i32_type();
    let ptr = cx.ptr();

    // Pass 1: create globals (types + declarations) so parents can be
    // referenced regardless of declaration order.
    let mut arts: Vec<(ClassArt, inkwell::values::GlobalValue)> = Vec::new();
    for (index, class) in program.classes.iter().enumerate() {
        let mut slot_tys: Vec<BasicTypeEnum> = std::iter::once(ptr.into())
            .chain(class.slots.iter().map(|&t| cx.basic_ty(t)))
            .collect();
        if class.is_dynamic {
            // Hidden expando-map slot (SPECS §3.2 dynamic classes).
            slot_tys.push(ptr.into());
        }
        let instance_ty = cx.context.struct_type(&slot_tys, false);
        let size = td.get_abi_size(&instance_ty);
        let expando_off: u32 = if class.is_dynamic {
            td.offset_of_element(&instance_ty, (slot_tys.len() - 1) as u32)
                .unwrap_or(0) as u32
        } else {
            u32::MAX
        };
        let vtable_ty = ptr.array_type(class.vtable.len() as u32);
        let rtti_ty = cx.context.struct_type(
            &[
                i32t.into(),      // type_id
                i32t.into(),      // pad
                ptr.into(),       // parent
                ptr.into(),       // name
                i32t.into(),      // iface_count
                i32t.into(),      // pad2
                ptr.into(),       // ifaces
                ptr.into(),       // to_string
                i32t.into(),      // expando byte offset (u32::MAX = sealed)
                i32t.into(),      // namespaced-member count (P16)
                ptr.into(),       // member table (VsMemberInfo entries)
                vtable_ty.into(), // vtable
            ],
            false,
        );
        let global = cx
            .module
            .add_global(rtti_ty, None, &format!("vs_rtti{index}"));
        let statics: Vec<PointerValue> = class
            .statics
            .iter()
            .enumerate()
            .map(|(si, &ty)| {
                let g =
                    cx.module
                        .add_global(cx.basic_ty(ty), None, &format!("vs_static{index}_{si}"));
                let init: BasicValueEnum = match ty {
                    Ty::Number => cx.context.f64_type().const_float(f64::NAN).into(),
                    t => cx.basic_ty(t).const_zero(),
                };
                g.set_initializer(&init);
                g.as_pointer_value()
            })
            .collect();
        arts.push((
            ClassArt {
                rtti: global.as_pointer_value(),
                rtti_ty,
                instance_ty,
                size,
                expando_off,
                statics,
            },
            global,
        ));
    }

    // Pass 2: initializers.
    for (index, class) in program.classes.iter().enumerate() {
        let (art, global) = &arts[index];
        // Qualified name as a constant VsString {i32 len, ptr data}.
        let units: Vec<u16> = class.name.encode_utf16().collect();
        let data_init: Vec<IntValue> = units
            .iter()
            .map(|&u| cx.context.i16_type().const_int(u64::from(u), false))
            .collect();
        let data_global = cx.module.add_global(
            cx.context.i16_type().array_type(units.len() as u32),
            None,
            &format!("vs_name_data{index}"),
        );
        data_global.set_initializer(&cx.context.i16_type().const_array(&data_init));
        data_global.set_constant(true);
        let str_ty = cx.context.struct_type(&[i32t.into(), ptr.into()], false);
        let name_global = cx
            .module
            .add_global(str_ty, None, &format!("vs_name{index}"));
        name_global.set_initializer(&str_ty.const_named_struct(&[
            i32t.const_int(units.len() as u64, false).into(),
            data_global.as_pointer_value().into(),
        ]));
        name_global.set_constant(true);

        // Interface tables + pair array.
        let pair_ty = cx
            .context
            .struct_type(&[i32t.into(), i32t.into(), ptr.into()], false);
        let mut pairs: Vec<inkwell::values::StructValue> = Vec::new();
        for (iface_id, table) in &class.ifaces {
            let entries: Vec<PointerValue> = table
                .iter()
                .map(|f| fns[f.0 as usize].as_global_value().as_pointer_value())
                .collect();
            let table_global = cx.module.add_global(
                ptr.array_type(entries.len() as u32),
                None,
                &format!("vs_itab{index}_{iface_id}"),
            );
            table_global.set_initializer(&ptr.const_array(&entries));
            table_global.set_constant(true);
            pairs.push(pair_ty.const_named_struct(&[
                i32t.const_int(u64::from(*iface_id), false).into(),
                i32t.const_zero().into(),
                table_global.as_pointer_value().into(),
            ]));
        }
        let ifaces_ptr: PointerValue = if pairs.is_empty() {
            ptr.const_null()
        } else {
            let pairs_global = cx.module.add_global(
                pair_ty.array_type(pairs.len() as u32),
                None,
                &format!("vs_ipairs{index}"),
            );
            pairs_global.set_initializer(&pair_ty.const_array(&pairs));
            pairs_global.set_constant(true);
            pairs_global.as_pointer_value()
        };

        let parent_ptr: PointerValue = match class.parent {
            Some(p) => arts[p as usize].0.rtti,
            None => ptr.const_null(),
        };
        let to_string_ptr: PointerValue = match class.to_string {
            Some(f) => fns[f.0 as usize].as_global_value().as_pointer_value(),
            None => ptr.const_null(),
        };
        let vtable_entries: Vec<PointerValue> = class
            .vtable
            .iter()
            .map(|f| fns[f.0 as usize].as_global_value().as_pointer_value())
            .collect();
        // Reflection table global: declared here so the descriptor can
        // point at it; rows are filled by emit_ns_member_tables once the
        // class artifacts (and thus Virtual wrappers) exist.
        let members_ptr: PointerValue = if class.ns_members.is_empty() {
            ptr.const_null()
        } else {
            let g = cx.module.add_global(
                member_info_ty(cx).array_type(class.ns_members.len() as u32),
                None,
                &format!("vs_members{index}"),
            );
            g.as_pointer_value()
        };
        let init = art.rtti_ty.const_named_struct(&[
            i32t.const_int(index as u64, false).into(),
            i32t.const_zero().into(),
            parent_ptr.into(),
            name_global.as_pointer_value().into(),
            i32t.const_int(class.ifaces.len() as u64, false).into(),
            i32t.const_zero().into(),
            ifaces_ptr.into(),
            to_string_ptr.into(),
            i32t.const_int(u64::from(art.expando_off), false).into(),
            i32t.const_int(class.ns_members.len() as u64, false).into(),
            members_ptr.into(),
            ptr.const_array(&vtable_entries).into(),
        ]);
        global.set_initializer(&init);
        global.set_constant(true);
    }
    arts.into_iter().map(|(a, _)| a).collect()
}

impl<'a, 'ctx> FnCx<'a, 'ctx> {
    /// Cell pointer for capture slot `i` of the current function.
    fn capture_cell(&mut self, i: usize) -> PointerValue<'ctx> {
        let cx = self.cx;
        let env = self.env_param.expect("capturing function has env");
        let slot = unsafe {
            self.cx.builder.build_in_bounds_gep(
                cx.ptr(),
                env,
                &[cx.context.i32_type().const_int(i as u64, false)],
                "cap",
            )
        }
        .expect("gep");
        self.cx
            .builder
            .build_load(cx.ptr(), slot, "capcell")
            .expect("load")
            .into_pointer_value()
    }

    fn make_closure(
        &mut self,
        wrapper: FunctionValue<'ctx>,
        envc: u32,
        env: PointerValue<'ctx>,
        this: PointerValue<'ctx>,
    ) -> Val<'ctx> {
        let cx = self.cx;
        let f = cx.runtime_fn(
            "vs_closure_new",
            Some(cx.ptr().into()),
            &[
                cx.ptr().into(),
                cx.context.i32_type().into(),
                cx.ptr().into(),
                cx.ptr().into(),
            ],
        );
        let call = self
            .cx
            .builder
            .build_call(
                f,
                &[
                    wrapper.as_global_value().as_pointer_value().into(),
                    cx.context
                        .i32_type()
                        .const_int(u64::from(envc), false)
                        .into(),
                    env.into(),
                    this.into(),
                ],
                "closure",
            )
            .expect("call");
        Val::Fun(
            call.try_as_basic_value()
                .basic()
                .expect("value")
                .into_pointer_value(),
        )
    }

    /// Indirect call of a Function value with the boxed ABI.
    fn call_fn_value(
        &mut self,
        callee: &mir::Expr,
        this_arg: Option<&mir::Expr>,
        args: &[mir::Expr],
        is_apply: bool,
        result_ty: Ty,
    ) -> Val<'ctx> {
        let cx = self.cx;
        let c = match self.expr(callee) {
            Val::Fun(c) => c,
            Val::Any(p) => {
                let f = cx.runtime_fn(
                    "vs_any_coerce_function",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into()],
                );
                self.cx
                    .builder
                    .build_call(f, &[p.into()], "fv")
                    .expect("call")
                    .try_as_basic_value()
                    .basic()
                    .expect("value")
                    .into_pointer_value()
            }
            _ => unreachable!("Function callee"),
        };
        // `this` argument: only object-shaped values bind (SPECS §3.7 —
        // primitives pass null).
        let this = match this_arg.map(|t| self.expr(t)) {
            Some(Val::Obj(p)) => p,
            Some(Val::Any(p)) => {
                // Extract object payloads; other tags become null.
                let f = cx.runtime_fn(
                    "vs_any_as_class",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into(), cx.ptr().into()],
                );
                let object_rtti = cx.classes[0].rtti;
                self.cx
                    .builder
                    .build_call(f, &[p.into(), object_rtti.into()], "thisobj")
                    .expect("call")
                    .try_as_basic_value()
                    .basic()
                    .expect("value")
                    .into_pointer_value()
            }
            _ => cx.ptr().const_null(),
        };
        let out = self.entry_alloca(cx.any_ty, "callout");
        if is_apply {
            let arr = match args.first().map(|a| self.expr(a)) {
                Some(Val::Arr(a)) => a,
                Some(Val::Str(a)) => a, // null literal carrier
                _ => cx.ptr().const_null(),
            };
            let f = cx.runtime_fn(
                "vs_closure_apply",
                None,
                &[
                    cx.ptr().into(),
                    cx.ptr().into(),
                    cx.ptr().into(),
                    cx.ptr().into(),
                ],
            );
            self.cx
                .builder
                .build_call(f, &[c.into(), this.into(), arr.into(), out.into()], "")
                .expect("call");
        } else {
            let staged: Vec<Option<&mir::Expr>> = args.iter().map(Some).collect();
            let buf = self.stage_any_array_opt(&staged);
            let f = cx.runtime_fn(
                "vs_closure_call",
                None,
                &[
                    cx.ptr().into(),
                    cx.ptr().into(),
                    cx.context.i32_type().into(),
                    cx.ptr().into(),
                    cx.ptr().into(),
                ],
            );
            let n = cx.context.i32_type().const_int(args.len() as u64, false);
            self.cx
                .builder
                .build_call(
                    f,
                    &[c.into(), this.into(), n.into(), buf.into(), out.into()],
                    "",
                )
                .expect("call");
        }
        self.unbox_any_ptr(out, result_ty)
    }

    /// Boxed-ABI wrapper for `fn_id` (memoized): unboxes declared params,
    /// calls the real function, boxes the result.
    fn wrapper_of(&mut self, fn_id: mir::FnId) -> FunctionValue<'ctx> {
        if let Some(w) = self.cx.wrappers.borrow().get(&fn_id.0) {
            return *w;
        }
        let w = build_wrapper(self.cx, self.fns, self.program, WrapTarget::Direct(fn_id));
        self.cx.wrappers.borrow_mut().insert(fn_id.0, w);
        w
    }

    /// Virtual-dispatch wrapper for bound methods (memoized per slot).
    fn vwrapper_of(&mut self, class: u32, vslot: usize) -> FunctionValue<'ctx> {
        if let Some(w) = self.cx.vwrappers.borrow().get(&(class, vslot)) {
            return *w;
        }
        let w = build_wrapper(
            self.cx,
            self.fns,
            self.program,
            WrapTarget::Virtual { class, vslot },
        );
        self.cx.vwrappers.borrow_mut().insert((class, vslot), w);
        w
    }

    fn builtin_wrapper(&mut self, b: mir::Builtin) -> FunctionValue<'ctx> {
        let key = b as u32;
        if let Some(w) = self.cx.bwrappers.borrow().get(&key) {
            return *w;
        }
        let w = build_wrapper(self.cx, self.fns, self.program, WrapTarget::Builtin(b));
        self.cx.bwrappers.borrow_mut().insert(key, w);
        w
    }
}

impl<'a, 'ctx> FnCx<'a, 'ctx> {
    /// A runtime string from a compile-time constant.
    fn const_string(&mut self, s: &str) -> PointerValue<'ctx> {
        let cx = self.cx;
        let global = self
            .cx
            .builder
            .build_global_string_ptr(s, "cstr")
            .expect("global");
        let f = cx.runtime_fn(
            "vs_string_from_utf8",
            Some(cx.ptr().into()),
            &[cx.ptr().into(), cx.context.i32_type().into()],
        );
        let len = cx.context.i32_type().const_int(s.len() as u64, false);
        self.cx
            .builder
            .build_call(f, &[global.as_pointer_value().into(), len.into()], "cs")
            .expect("call")
            .try_as_basic_value()
            .basic()
            .expect("value")
            .into_pointer_value()
    }

    /// Native static dispatch (SPECS §6 P7 surfaces). Math routes to libm /
    /// folded pairs; the rest are runtime calls.
    fn call_native(&mut self, nf: mir::SemaNativeFn, args: &[mir::Expr], ret: Ty) -> Val<'ctx> {
        use mir::SemaNativeFn as N;
        let cx = self.cx;
        let f64t = cx.context.f64_type();
        let libm1 = |name: &str| name.to_string();
        match nf {
            N::MathAbs
            | N::MathCeil
            | N::MathFloor
            | N::MathSqrt
            | N::MathExp
            | N::MathLog
            | N::MathSin
            | N::MathCos
            | N::MathTan
            | N::MathAsin
            | N::MathAcos
            | N::MathAtan => {
                let name = match nf {
                    N::MathAbs => libm1("fabs"),
                    N::MathCeil => libm1("ceil"),
                    N::MathFloor => libm1("floor"),
                    N::MathSqrt => libm1("sqrt"),
                    N::MathExp => libm1("exp"),
                    N::MathLog => libm1("log"),
                    N::MathSin => libm1("sin"),
                    N::MathCos => libm1("cos"),
                    N::MathTan => libm1("tan"),
                    N::MathAsin => libm1("asin"),
                    N::MathAcos => libm1("acos"),
                    _ => libm1("atan"),
                };
                let x = self.num_arg(&args[0]);
                let f = cx.runtime_fn(&name, Some(f64t.into()), &[f64t.into()]);
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[x.into()], "m")
                    .expect("call");
                Val::Num(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_float_value(),
                )
            }
            N::MathPow | N::MathAtan2 => {
                let name = if nf == N::MathPow { "pow" } else { "atan2" };
                let a = self.num_arg(&args[0]);
                let b = self.num_arg(&args[1]);
                let f = cx.runtime_fn(name, Some(f64t.into()), &[f64t.into(), f64t.into()]);
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[a.into(), b.into()], "m2")
                    .expect("call");
                Val::Num(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_float_value(),
                )
            }
            // §15.8.2.15: round = floor(x + 0.5).
            N::MathRound => {
                let x = self.num_arg(&args[0]);
                let half = f64t.const_float(0.5);
                let sum = self.cx.builder.build_float_add(x, half, "rh").expect("add");
                let f = cx.runtime_fn("floor", Some(f64t.into()), &[f64t.into()]);
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[sum.into()], "round")
                    .expect("call");
                Val::Num(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_float_value(),
                )
            }
            // §15.8.2.11/12: fold pairwise, NaN propagates (runtime pair fn).
            N::MathMin | N::MathMax => {
                let name = if nf == N::MathMin {
                    "vs_math_min2"
                } else {
                    "vs_math_max2"
                };
                let f = cx.runtime_fn(name, Some(f64t.into()), &[f64t.into(), f64t.into()]);
                let mut acc = self.num_arg(&args[0]);
                for a in &args[1..] {
                    let b = self.num_arg(a);
                    acc = self
                        .cx
                        .builder
                        .build_call(f, &[acc.into(), b.into()], "mm")
                        .expect("call")
                        .try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_float_value();
                }
                Val::Num(acc)
            }
            N::MathRandom => {
                let f = cx.runtime_fn("vs_math_random", Some(f64t.into()), &[]);
                let call = self.cx.builder.build_call(f, &[], "rnd").expect("call");
                Val::Num(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_float_value(),
                )
            }
            N::SystemArgs => {
                let f = cx.runtime_fn("vs_system_args", Some(cx.ptr().into()), &[]);
                let call = self.cx.builder.build_call(f, &[], "argv").expect("call");
                Val::Arr(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            N::SystemExit => {
                let code = match self.expr(&args[0]) {
                    Val::Int(i) | Val::UInt(i) => i,
                    _ => unreachable!("exit code"),
                };
                let f = cx.runtime_fn("vs_system_exit", None, &[cx.context.i32_type().into()]);
                self.cx
                    .builder
                    .build_call(f, &[code.into()], "")
                    .expect("call");
                self.cx.builder.build_unreachable().expect("unreachable");
                let dead = self.new_block("after.exit");
                self.cx.builder.position_at_end(dead);
                Val::Void
            }
            N::SocketConnect => {
                let host = self.str_arg(&args[0]);
                let port = match self.expr(&args[1]) {
                    Val::Int(i) | Val::UInt(i) => i,
                    _ => unreachable!("port"),
                };
                let f = cx.runtime_fn(
                    "vs_socket_connect",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into(), cx.context.i32_type().into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[host.into(), port.into()], "sock")
                    .expect("call");
                Val::Sock(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            N::ServerSocketBind => {
                let port = match self.expr(&args[0]) {
                    Val::Int(i) | Val::UInt(i) => i,
                    _ => unreachable!("port"),
                };
                let f = cx.runtime_fn(
                    "vs_socket_bind",
                    Some(cx.ptr().into()),
                    &[cx.context.i32_type().into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[port.into()], "srv")
                    .expect("call");
                Val::Sock(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            N::DateUTC => {
                let arr = self.stage_f64_args(args, "utcparts");
                let i32t = cx.context.i32_type();
                let f64t = cx.context.f64_type();
                let f = cx.runtime_fn(
                    "vs_date_utc",
                    Some(f64t.into()),
                    &[i32t.into(), cx.ptr().into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(
                        f,
                        &[i32t.const_int(args.len() as u64, false).into(), arr.into()],
                        "utc",
                    )
                    .expect("call");
                Val::Num(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_float_value(),
                )
            }
            N::SystemGc => {
                let f = cx.runtime_fn("vs_gc_collect", None, &[]);
                self.cx.builder.build_call(f, &[], "").expect("call");
                Val::Void
            }
            N::SystemGcLiveBytes => {
                let f = cx.runtime_fn("vs_gc_live_bytes", Some(cx.context.f64_type().into()), &[]);
                let call = self.cx.builder.build_call(f, &[], "live").expect("call");
                Val::Num(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_float_value(),
                )
            }
            N::SystemGetenv => {
                let name = self.str_arg(&args[0]);
                let f = cx.runtime_fn(
                    "vs_system_getenv",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[name.into()], "env")
                    .expect("call");
                Val::Str(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            N::SystemTime | N::DateNow => {
                let f = cx.runtime_fn("vs_system_time", Some(f64t.into()), &[]);
                let call = self.cx.builder.build_call(f, &[], "ms").expect("call");
                Val::Num(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_float_value(),
                )
            }
            N::FileRead => {
                let path = self.str_arg(&args[0]);
                let f = cx.runtime_fn("vs_file_read", Some(cx.ptr().into()), &[cx.ptr().into()]);
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[path.into()], "fr")
                    .expect("call");
                Val::Str(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            N::FileWrite => {
                let path = self.str_arg(&args[0]);
                let text = self.str_arg(&args[1]);
                let f = cx.runtime_fn(
                    "vs_file_write",
                    Some(cx.context.i32_type().into()),
                    &[cx.ptr().into(), cx.ptr().into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[path.into(), text.into()], "fw")
                    .expect("call");
                Val::Bool(
                    self.nonzero(
                        call.try_as_basic_value()
                            .basic()
                            .expect("value")
                            .into_int_value(),
                    ),
                )
            }
            // P18 File IO expansion: one-path and two-path Boolean ops
            // share shapes; list returns Array (null = error), size/mtime
            // return Number.
            N::FileRemove | N::FileMkdir | N::FileRmdir | N::FileIsDirectory => {
                let name = match nf {
                    N::FileRemove => "vs_file_remove",
                    N::FileMkdir => "vs_file_mkdir",
                    N::FileRmdir => "vs_file_rmdir",
                    _ => "vs_file_is_directory",
                };
                let path = self.str_arg(&args[0]);
                let f = cx.runtime_fn(name, Some(cx.context.i32_type().into()), &[cx.ptr().into()]);
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[path.into()], "fio")
                    .expect("call");
                Val::Bool(
                    self.nonzero(
                        call.try_as_basic_value()
                            .basic()
                            .expect("value")
                            .into_int_value(),
                    ),
                )
            }
            N::FileAppend | N::FileCopy | N::FileRename => {
                let name = match nf {
                    N::FileAppend => "vs_file_append",
                    N::FileCopy => "vs_file_copy",
                    _ => "vs_file_rename",
                };
                let a = self.str_arg(&args[0]);
                let b = self.str_arg(&args[1]);
                let f = cx.runtime_fn(
                    name,
                    Some(cx.context.i32_type().into()),
                    &[cx.ptr().into(), cx.ptr().into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[a.into(), b.into()], "fio2")
                    .expect("call");
                Val::Bool(
                    self.nonzero(
                        call.try_as_basic_value()
                            .basic()
                            .expect("value")
                            .into_int_value(),
                    ),
                )
            }
            N::FileList => {
                let path = self.str_arg(&args[0]);
                let f = cx.runtime_fn("vs_file_list", Some(cx.ptr().into()), &[cx.ptr().into()]);
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[path.into()], "flist")
                    .expect("call");
                Val::Arr(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            N::FileSize | N::FileMtime => {
                let name = if nf == N::FileSize {
                    "vs_file_size"
                } else {
                    "vs_file_mtime"
                };
                let path = self.str_arg(&args[0]);
                let f = cx.runtime_fn(name, Some(cx.context.f64_type().into()), &[cx.ptr().into()]);
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[path.into()], "fstat")
                    .expect("call");
                Val::Num(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_float_value(),
                )
            }
            N::SystemReadLine => {
                let f = cx.runtime_fn("vs_system_read_line", Some(cx.ptr().into()), &[]);
                let call = self.cx.builder.build_call(f, &[], "rl").expect("call");
                Val::Str(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            N::FileExists => {
                let path = self.str_arg(&args[0]);
                let f = cx.runtime_fn(
                    "vs_file_exists",
                    Some(cx.context.i32_type().into()),
                    &[cx.ptr().into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[path.into()], "fe")
                    .expect("call");
                Val::Bool(
                    self.nonzero(
                        call.try_as_basic_value()
                            .basic()
                            .expect("value")
                            .into_int_value(),
                    ),
                )
            }
            N::JsonStringify => {
                let v = self.as_any_ptr(&args[0]);
                let f = cx.runtime_fn(
                    "vs_json_stringify",
                    Some(cx.ptr().into()),
                    &[cx.ptr().into()],
                );
                let call = self
                    .cx
                    .builder
                    .build_call(f, &[v.into()], "js")
                    .expect("call");
                Val::Str(
                    call.try_as_basic_value()
                        .basic()
                        .expect("value")
                        .into_pointer_value(),
                )
            }
            N::JsonParse => {
                let text = self.str_arg(&args[0]);
                let art = &cx.classes[0];
                let (rtti, size) = (art.rtti, art.size);
                let out = self.entry_alloca(cx.any_ty, "parsed");
                let f = cx.runtime_fn(
                    "vs_json_parse",
                    None,
                    &[
                        cx.ptr().into(),
                        cx.ptr().into(),
                        cx.context.i64_type().into(),
                        cx.ptr().into(),
                    ],
                );
                self.cx
                    .builder
                    .build_call(
                        f,
                        &[
                            text.into(),
                            rtti.into(),
                            cx.context.i64_type().const_int(size, false).into(),
                            out.into(),
                        ],
                        "",
                    )
                    .expect("call");
                let _ = ret;
                Val::Any(out)
            }
        }
    }
}

/// What a boxed-ABI wrapper forwards to.
enum WrapTarget {
    Direct(mir::FnId),
    Virtual { class: u32, vslot: usize },
    Builtin(mir::Builtin),
}

/// Synthesizes `fn(env, this, argc, args, out)` forwarding to the target
/// (closure ABI — see runtime/src/closure.rs).
/// The VsMemberInfo row type (namespace.rs layout contract).
fn member_info_ty<'ctx>(cx: &Cx<'ctx>) -> inkwell::types::StructType<'ctx> {
    let i32t = cx.context.i32_type();
    let i64t = cx.context.i64_type();
    let ptr = cx.ptr();
    cx.context.struct_type(
        &[
            ptr.into(),
            ptr.into(),
            i32t.into(),
            i32t.into(),
            i64t.into(),
        ],
        false,
    )
}

/// Fills the per-class reflection tables (declared during artifact
/// building): field offsets from the instance layout, method rows through
/// Virtual wrappers — which need `cx.classes`, hence this separate pass.
fn emit_ns_member_tables<'ctx>(
    cx: &Cx<'ctx>,
    program: &mir::Program,
    fns: &[FunctionValue<'ctx>],
    td: &inkwell::targets::TargetData,
) {
    let i32t = cx.context.i32_type();
    let i64t = cx.context.i64_type();
    let ptr = cx.ptr();
    let member_ty = member_info_ty(cx);
    let str_ty = cx.context.struct_type(&[i32t.into(), ptr.into()], false);
    let make_str = |text: &str, sym: &str| -> PointerValue<'ctx> {
        let units: Vec<u16> = text.encode_utf16().collect();
        let data: Vec<IntValue> = units
            .iter()
            .map(|&u| cx.context.i16_type().const_int(u64::from(u), false))
            .collect();
        let dg = cx.module.add_global(
            cx.context.i16_type().array_type(units.len() as u32),
            None,
            &format!("{sym}_data"),
        );
        dg.set_initializer(&cx.context.i16_type().const_array(&data));
        dg.set_constant(true);
        let g = cx.module.add_global(str_ty, None, sym);
        g.set_initializer(&str_ty.const_named_struct(&[
            i32t.const_int(units.len() as u64, false).into(),
            dg.as_pointer_value().into(),
        ]));
        g.set_constant(true);
        g.as_pointer_value()
    };
    for (index, class) in program.classes.iter().enumerate() {
        if class.ns_members.is_empty() {
            continue;
        }
        let rows: Vec<inkwell::values::StructValue> = class
            .ns_members
            .iter()
            .enumerate()
            .map(|(mi, m)| {
                let uri = make_str(
                    &program.namespace_uris[m.ns as usize],
                    &format!("vs_nsuri{index}_{mi}"),
                );
                let name = make_str(&m.name, &format!("vs_nsname{index}_{mi}"));
                let (kind, type_tag, payload): (u64, u64, IntValue) =
                    if let (Some(slot), Some(fty)) = (m.field_slot, m.field_ty) {
                        // Slot i lives at struct field i+1 (header word 0).
                        let off = td
                            .offset_of_element(&cx.classes[index].instance_ty, slot + 1)
                            .unwrap_or(0);
                        (
                            0,
                            u64::from(ty_tag_reflect(fty)),
                            i64t.const_int(off, false),
                        )
                    } else {
                        let vslot = m.vslot.expect("method row");
                        let w = build_wrapper(
                            cx,
                            fns,
                            program,
                            WrapTarget::Virtual {
                                class: index as u32,
                                vslot: vslot as usize,
                            },
                        );
                        (
                            1,
                            0,
                            w.as_global_value().as_pointer_value().const_to_int(i64t),
                        )
                    };
                member_ty.const_named_struct(&[
                    uri.into(),
                    name.into(),
                    i32t.const_int(kind, false).into(),
                    i32t.const_int(type_tag, false).into(),
                    payload.into(),
                ])
            })
            .collect();
        let g = cx
            .module
            .get_global(&format!("vs_members{index}"))
            .expect("declared member table");
        g.set_initializer(&member_ty.const_array(&rows));
        g.set_constant(true);
    }
}

fn build_wrapper<'ctx>(
    cx: &Cx<'ctx>,
    fns: &[FunctionValue<'ctx>],
    program: &mir::Program,
    target: WrapTarget,
) -> FunctionValue<'ctx> {
    let saved = cx.builder.get_insert_block();
    let i32t = cx.context.i32_type();
    let ptr = cx.ptr();
    let wrap_ty = cx.context.void_type().fn_type(
        &[ptr.into(), ptr.into(), i32t.into(), ptr.into(), ptr.into()],
        false,
    );
    let name = match &target {
        WrapTarget::Direct(f) => format!("vs_wrap{}", f.0),
        WrapTarget::Virtual { class, vslot } => format!("vs_vwrap{class}_{vslot}"),
        WrapTarget::Builtin(b) => format!("vs_bwrap{}", *b as u32),
    };
    let wrapper = cx.module.add_function(&name, wrap_ty, None);
    let entry = cx.context.append_basic_block(wrapper, "entry");
    let body = cx.context.append_basic_block(wrapper, "body");
    cx.builder.position_at_end(body);

    // A minimal FnCx over the wrapper for the unbox/box helpers. The
    // referenced mir function is only used for parameter metadata.
    let meta_fn = match &target {
        WrapTarget::Direct(f) => &program.functions[f.0 as usize],
        WrapTarget::Virtual { class, vslot } => {
            let f = program.classes[*class as usize].vtable[*vslot];
            &program.functions[f.0 as usize]
        }
        WrapTarget::Builtin(_) => &program.functions[0],
    };
    let mut fcx = FnCx {
        cx,
        fns,
        program,
        function: wrapper,
        mir_fn: meta_fn,
        locals: Vec::new(),
        frames: Vec::new(),
        entry,
        env_param: None,
        exit_actions: Vec::new(),
    };
    let env = wrapper.get_nth_param(0).unwrap().into_pointer_value();
    let this = wrapper.get_nth_param(1).unwrap().into_pointer_value();
    let argc = wrapper.get_nth_param(2).unwrap().into_int_value();
    let args = wrapper.get_nth_param(3).unwrap().into_pointer_value();
    let out = wrapper.get_nth_param(4).unwrap().into_pointer_value();

    match target {
        WrapTarget::Builtin(b) => {
            // trace forwards boxed args directly; the numeric builtins
            // unbox one/two fixed params.
            match b {
                mir::Builtin::Trace => {
                    let f = cx.runtime_fn("vs_trace", None, &[i32t.into(), ptr.into()]);
                    cx.builder
                        .build_call(f, &[argc.into(), args.into()], "")
                        .expect("call");
                    let _ = out;
                }
                _ => {
                    // Unbox arg0 (+ radix for parseInt) and forward.
                    let a0 = fcx.entry_alloca(cx.any_ty, "a0");
                    let loaded = cx.builder.build_load(cx.any_ty, args, "l0").expect("load");
                    cx.builder.build_store(a0, loaded).expect("store");
                    let result: Val = match b {
                        mir::Builtin::ParseInt => {
                            let sv = fcx.unbox_any_ptr(a0, Ty::String);
                            let Val::Str(sp) = sv else { unreachable!() };
                            let f = cx.runtime_fn(
                                "vs_parse_int",
                                Some(cx.context.f64_type().into()),
                                &[ptr.into(), i32t.into()],
                            );
                            let call = cx
                                .builder
                                .build_call(f, &[sp.into(), i32t.const_zero().into()], "pi")
                                .expect("call");
                            Val::Num(
                                call.try_as_basic_value()
                                    .basic()
                                    .expect("value")
                                    .into_float_value(),
                            )
                        }
                        mir::Builtin::ParseFloat => {
                            let sv = fcx.unbox_any_ptr(a0, Ty::String);
                            let Val::Str(sp) = sv else { unreachable!() };
                            let f = cx.runtime_fn(
                                "vs_parse_float",
                                Some(cx.context.f64_type().into()),
                                &[ptr.into()],
                            );
                            let call = cx.builder.build_call(f, &[sp.into()], "pf").expect("call");
                            Val::Num(
                                call.try_as_basic_value()
                                    .basic()
                                    .expect("value")
                                    .into_float_value(),
                            )
                        }
                        _ => {
                            // isNaN / isFinite
                            let n = fcx.any_to_number(a0);
                            let b_ = &cx.builder;
                            let flag = match b {
                                mir::Builtin::IsNaN => b_
                                    .build_float_compare(FloatPredicate::UNO, n, n, "isnan")
                                    .expect("cmp"),
                                _ => {
                                    let sub = b_.build_float_sub(n, n, "ss").expect("sub");
                                    b_.build_float_compare(
                                        FloatPredicate::OEQ,
                                        sub,
                                        cx.context.f64_type().const_zero(),
                                        "fin",
                                    )
                                    .expect("cmp")
                                }
                            };
                            Val::Bool(flag)
                        }
                    };
                    let boxed = fcx.box_value(result);
                    let v = cx
                        .builder
                        .build_load(cx.any_ty, boxed, "res")
                        .expect("load");
                    cx.builder.build_store(out, v).expect("store");
                }
            }
        }
        WrapTarget::Direct(fn_id) => {
            let callee = fns[fn_id.0 as usize];
            let mf = &program.functions[fn_id.0 as usize];
            let mut argv: Vec<BasicMetadataValueEnum> = Vec::new();
            if !mf.captures.is_empty() {
                argv.push(env.into());
            }
            if mf.this_class.is_some() {
                argv.push(this.into());
            }
            for i in 0..mf.param_count {
                let v = wrapper_param(&mut fcx, argc, args, i, mf.locals[i]);
                argv.push(fcx.materialize(v).into());
            }
            let call = cx.builder.build_call(callee, &argv, "fwd").expect("call");
            store_wrapper_result(&mut fcx, call, mf.ret, out);
        }
        WrapTarget::Virtual { class, vslot } => {
            let target_fn = program.classes[class as usize].vtable[vslot];
            let mf = &program.functions[target_fn.0 as usize];
            // Virtual dispatch on the bound `this`.
            let desc = cx
                .builder
                .build_load(ptr, this, "desc")
                .expect("load")
                .into_pointer_value();
            let rtti_ty = cx.classes[class as usize].rtti_ty;
            let vt = cx
                .builder
                .build_struct_gep(rtti_ty, desc, 11, "vt")
                .expect("gep");
            let slot_ptr = unsafe {
                cx.builder.build_in_bounds_gep(
                    ptr.array_type(0),
                    vt,
                    &[i32t.const_zero(), i32t.const_int(vslot as u64, false)],
                    "vslot",
                )
            }
            .expect("gep");
            let fptr = cx
                .builder
                .build_load(ptr, slot_ptr, "vfn")
                .expect("load")
                .into_pointer_value();
            let mut param_tys: Vec<BasicMetadataTypeEnum> = vec![ptr.into()];
            let mut argv: Vec<BasicMetadataValueEnum> = vec![this.into()];
            for i in 0..mf.param_count {
                param_tys.push(cx.basic_ty(mf.locals[i]).into());
                let v = wrapper_param(&mut fcx, argc, args, i, mf.locals[i]);
                argv.push(fcx.materialize(v).into());
            }
            let fn_ty = match mf.ret {
                Ty::Void => cx.context.void_type().fn_type(&param_tys, false),
                t => cx.basic_ty(t).fn_type(&param_tys, false),
            };
            let call = cx
                .builder
                .build_indirect_call(fn_ty, fptr, &argv, "vfwd")
                .expect("call");
            store_wrapper_result(&mut fcx, call, mf.ret, out);
        }
    }
    cx.builder.build_return(None).expect("ret");
    cx.builder.position_at_end(entry);
    cx.builder.build_unconditional_branch(body).expect("br");
    if let Some(bb) = saved {
        cx.builder.position_at_end(bb);
    }
    wrapper
}

/// Wrapper parameter i: unboxed from `args` when provided, else the type
/// default (defaults-as-expressions are evaluated at typed callsites; the
/// boxed ABI falls back to type defaults — documented deviation).
fn wrapper_param<'ctx>(
    fcx: &mut FnCx<'_, 'ctx>,
    argc: IntValue<'ctx>,
    args: PointerValue<'ctx>,
    i: usize,
    ty: Ty,
) -> Val<'ctx> {
    let cx = fcx.cx;
    let i32t = cx.context.i32_type();
    let have = cx
        .builder
        .build_int_compare(
            IntPredicate::UGT,
            argc,
            i32t.const_int(i as u64, false),
            "have",
        )
        .expect("cmp");
    let slot = fcx.entry_alloca(cx.any_ty, "argslot");
    let then_bb = cx.context.append_basic_block(fcx.function, "arg.have");
    let else_bb = cx.context.append_basic_block(fcx.function, "arg.dflt");
    let end_bb = cx.context.append_basic_block(fcx.function, "arg.end");
    cx.builder
        .build_conditional_branch(have, then_bb, else_bb)
        .expect("br");
    cx.builder.position_at_end(then_bb);
    let src = unsafe {
        cx.builder
            .build_in_bounds_gep(cx.any_ty, args, &[i32t.const_int(i as u64, false)], "argp")
    }
    .expect("gep");
    let v = cx.builder.build_load(cx.any_ty, src, "argv").expect("load");
    cx.builder.build_store(slot, v).expect("store");
    cx.builder.build_unconditional_branch(end_bb).expect("br");
    cx.builder.position_at_end(else_bb);
    cx.builder
        .build_store(slot, cx.any_ty.const_zero())
        .expect("store");
    cx.builder.build_unconditional_branch(end_bb).expect("br");
    cx.builder.position_at_end(end_bb);
    let unboxed = fcx.unbox_any_ptr(slot, ty);
    // Undefined unboxes: numeric → NaN via ToNumber; acceptable defaults.
    unboxed
}

fn store_wrapper_result<'ctx>(
    fcx: &mut FnCx<'_, 'ctx>,
    call: inkwell::values::CallSiteValue<'ctx>,
    ret: Ty,
    out: PointerValue<'ctx>,
) {
    let cx = fcx.cx;
    let result = match ret {
        Ty::Void => {
            cx.builder
                .build_store(out, cx.any_ty.const_zero())
                .expect("store");
            return;
        }
        t => fcx.wrap_basic(call.try_as_basic_value().basic().expect("value"), t),
    };
    let boxed = fcx.box_value(result);
    let v = cx
        .builder
        .build_load(cx.any_ty, boxed, "resv")
        .expect("load");
    cx.builder.build_store(out, v).expect("store");
}

enum ArgKind {
    Num,
    Str,
}

/// Whether evaluating this expression can allocate a GC block directly
/// (drives safepoint elision, P22). Calls to compiled functions do NOT
/// count — an allocating callee keeps its own entry safepoint, so the
/// debt-to-collection delay stays bounded inductively. Runtime natives
/// and string/collection operations DO count: they allocate inside the
/// runtime, which carries no safepoints of its own.
fn expr_allocs(e: &mir::Expr) -> bool {
    use mir::ExprKind as E;
    match &e.kind {
        // Pure leaves.
        E::Int(_)
        | E::UInt(_)
        | E::Number(_)
        | E::Bool(_)
        | E::Null
        | E::Undefined
        | E::LocalGet(_)
        | E::CaptureGet(_)
        | E::StaticGet(..)
        | E::This
        | E::IncDec { .. }
        | E::StaticIncDec { .. }
        | E::CaptureIncDec { .. } => false,
        // Allocating leaves / constructions (short-circuit true).
        E::Str(_)
        | E::ArrayLit(_)
        | E::VectorLit(..)
        | E::ObjectLit(_)
        | E::RegExpLit(..)
        | E::NewRegExp(_)
        | E::NewDate(_)
        | E::NewNamespace(_)
        | E::NamespaceVal(_)
        | E::New(..)
        | E::Closure(_)
        | E::FnValue(_)
        | E::BuiltinValue(_)
        | E::BoundMethod(..)
        | E::CallBuiltin(..)
        | E::CallStrMethod(..)
        | E::CallNumMethod(..)
        | E::CallArr(..)
        | E::CallVec(..)
        | E::CallNative(..)
        | E::CallRegex(..)
        | E::CallDate(..)
        | E::CallSocket(..)
        | E::NsGet(..)
        | E::NsCall(..)
        | E::NsUri(_)
        | E::EnumKey(..)
        | E::EnumValue(..)
        | E::PropGet(..)
        | E::PropSet(..)
        | E::PropCall(..)
        | E::AnyIndexGet(..)
        | E::AnyIndexSet(..)
        | E::HasProp(..)
        | E::DeleteProp(..) => true,
        // Compiled-code calls: the callee covers itself; scan arguments.
        E::CallFn(_, args) => args.iter().any(expr_allocs),
        E::CallVirtual { recv, args, .. }
        | E::CallIface { recv, args, .. }
        | E::CallDirect { recv, args, .. } => {
            expr_allocs(recv) || args.iter().any(expr_allocs)
        }
        E::CallFnValue {
            callee,
            this_arg,
            args,
            ..
        } => {
            expr_allocs(callee)
                || this_arg.as_deref().is_some_and(expr_allocs)
                || args.iter().any(expr_allocs)
        }
        // String concatenation / boxed arithmetic build result values.
        E::Binary(_, a, b) => {
            e.ty == Ty::String
                || a.ty == Ty::Any
                || b.ty == Ty::Any
                || expr_allocs(a)
                || expr_allocs(b)
        }
        E::Conv(c, v) => {
            matches!(c, Conv::ToString | Conv::AnyToString) || expr_allocs(v)
        }
        // Pure-ish wrappers: whatever the children do.
        E::LocalSet(_, v)
        | E::CaptureSet(_, v)
        | E::StaticSet(_, _, v)
        | E::Unary(_, v)
        | E::Is(v, _)
        | E::As(v, _)
        | E::StrLen(v)
        | E::SeqLen(v)
        | E::EnumLen(v) => expr_allocs(v),
        E::FieldIncDec { recv, .. } => expr_allocs(recv),
        E::FieldGet(v, ..) => expr_allocs(v),
        E::FieldSet(o, _, _, v)
        | E::SeqSetLen(o, v)
        | E::Comma(o, v) => expr_allocs(o) || expr_allocs(v),
        E::SeqGet(o, v, _) => expr_allocs(o) || expr_allocs(v),
        E::Logical { lhs, rhs, .. } => expr_allocs(lhs) || expr_allocs(rhs),
        E::Conditional(a, b, c) => expr_allocs(a) || expr_allocs(b) || expr_allocs(c),
        E::SeqSet(a, b, c, _) => expr_allocs(a) || expr_allocs(b) || expr_allocs(c),
    }
}

/// Whether any statement in the tree allocates directly (loop bodies
/// included — the loop-level analysis calls this on its own body).
fn stmts_alloc(stmts: &[mir::Stmt]) -> bool {
    stmts.iter().any(stmt_allocs)
}

fn stmt_allocs(s: &mir::Stmt) -> bool {
    use mir::Stmt as S;
    match s {
        S::Expr(e) | S::Assign(_, e) | S::Throw(e) => expr_allocs(e),
        S::Block(b) => stmts_alloc(b),
        S::If {
            cond,
            then_branch,
            else_branch,
        } => {
            expr_allocs(cond)
                || stmt_allocs(then_branch)
                || else_branch.as_deref().is_some_and(stmt_allocs)
        }
        S::While { cond, body, .. } | S::DoWhile { cond, body, .. } => {
            expr_allocs(cond) || stmt_allocs(body)
        }
        S::For {
            init,
            cond,
            update,
            body,
            ..
        } => {
            init.as_deref().is_some_and(stmt_allocs)
                || cond.as_ref().is_some_and(expr_allocs)
                || update.as_ref().is_some_and(expr_allocs)
                || stmt_allocs(body)
        }
        S::Switch { scrutinee, cases } => {
            expr_allocs(scrutinee)
                || cases
                    .iter()
                    .any(|c| c.test.as_ref().is_some_and(expr_allocs) || stmts_alloc(&c.body))
        }
        S::Return { value } => value.as_ref().is_some_and(expr_allocs),
        S::Try {
            body,
            catches,
            finally,
        } => {
            // try-functions are optnone anyway; keep it conservative.
            stmts_alloc(body)
                || catches.iter().any(|c| stmts_alloc(&c.body))
                || finally.as_deref().is_some_and(stmts_alloc)
        }
        S::Break { .. } | S::Continue { .. } | S::Empty => false,
    }
}

/// Whether a statement tree contains a `try` (drives the optnone pin —
/// see FnCx::emit).
fn contains_try(stmts: &[mir::Stmt]) -> bool {
    use mir::Stmt as S;
    stmts.iter().any(|s| match s {
        S::Try { .. } => true,
        S::Block(b) => contains_try(b),
        S::If {
            then_branch,
            else_branch,
            ..
        } => {
            contains_try(std::slice::from_ref(then_branch))
                || else_branch
                    .as_deref()
                    .is_some_and(|e| contains_try(std::slice::from_ref(e)))
        }
        S::While { body, .. } | S::DoWhile { body, .. } => contains_try(std::slice::from_ref(body)),
        S::For { init, body: b, .. } => {
            init.as_deref()
                .is_some_and(|i| contains_try(std::slice::from_ref(i)))
                || contains_try(std::slice::from_ref(b))
        }
        S::Switch { cases, .. } => cases.iter().any(|c| contains_try(&c.body)),
        _ => false,
    })
}

/// VsAny tag a field slot boxes to (reflection tables; 0 = `*` slot).
fn ty_tag_reflect(ty: Ty) -> u32 {
    match ty {
        Ty::Any | Ty::Void => 0,
        Ty::Boolean => 2,
        Ty::Int => 3,
        Ty::UInt => 4,
        Ty::Number => 5,
        Ty::String => 6,
        Ty::Object(_) | Ty::Iface(_) => 7,
        Ty::Array => 8,
        Ty::Vector(_) => 9,
        Ty::Function => 10,
        Ty::RegExp => 11,
        Ty::Date => 12,
        Ty::Socket => 13,
        Ty::Namespace => 14,
    }
}

fn ty_tag(ty: Ty) -> u32 {
    match ty {
        Ty::Int => tag::INT,
        Ty::UInt => tag::UINT,
        Ty::Number => tag::NUMBER,
        Ty::Boolean => tag::BOOLEAN,
        Ty::String => tag::STRING,
        Ty::RegExp => tag::REGEXP,
        Ty::Date => tag::DATE,
        Ty::Socket => tag::SOCKET,
        Ty::Namespace => tag::NAMESPACE,
        // Class/interface/sequence targets dispatch through dedicated
        // runtime calls, never through core tags.
        Ty::Any
        | Ty::Void
        | Ty::Object(_)
        | Ty::Iface(_)
        | Ty::Array
        | Ty::Vector(_)
        | Ty::Function => tag::NULL,
    }
}

#[cfg(test)]
mod tests {
    use inkwell::context::Context;

    /// Smoke test: proves the inkwell → llvm-sys → brew LLVM 22 link works
    /// end to end, so toolchain breakage surfaces here first.
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
