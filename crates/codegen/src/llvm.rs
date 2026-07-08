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
        let module = context.create_module("vigor");
        let machine = target_machine(opts).map_err(|m| {
            vec![Diagnostic::error(
                ErrorCode::NOT_IMPLEMENTED,
                format!("cannot initialize LLVM target: {m}"),
            )]
        })?;
        module.set_triple(&machine.get_triple());

        let cx = Cx {
            context: &context,
            module,
            builder: context.create_builder(),
            any_ty: context.struct_type(
                &[context.i32_type().into(), context.i64_type().into()],
                false,
            ),
        };

        // Declare every function first (mutual recursion), then emit bodies.
        let fns: Vec<FunctionValue> = program
            .functions
            .iter()
            .enumerate()
            .map(|(i, f)| cx.declare_function(i, f))
            .collect();
        for (f, decl) in program.functions.iter().zip(&fns) {
            FnCx::emit(&cx, &fns, f, *decl);
        }

        if let Err(e) = cx.module.verify() {
            // A verifier failure is a compiler bug, not a user error — but
            // never panic on the user (CLAUDE.md §4): report it.
            return Err(vec![Diagnostic::error(
                ErrorCode::NOT_IMPLEMENTED,
                format!("internal codegen error (LLVM verifier): {}", e.to_string()),
            )]);
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
            OptimizationLevel::Default,
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
        let params: Vec<BasicMetadataTypeEnum> = f.locals[..f.param_count]
            .iter()
            .map(|&t| self.basic_ty(t).into())
            .collect();
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
}

/// Per-function emission state.
struct FnCx<'a, 'ctx> {
    cx: &'a Cx<'ctx>,
    fns: &'a [FunctionValue<'ctx>],
    function: FunctionValue<'ctx>,
    mir_fn: &'a mir::Function,
    locals: Vec<(PointerValue<'ctx>, Ty)>,
    frames: Vec<Frame<'ctx>>,
    entry: BasicBlock<'ctx>,
}

impl<'a, 'ctx> FnCx<'a, 'ctx> {
    fn emit(
        cx: &'a Cx<'ctx>,
        fns: &'a [FunctionValue<'ctx>],
        mir_fn: &'a mir::Function,
        function: FunctionValue<'ctx>,
    ) {
        let entry = cx.context.append_basic_block(function, "entry");
        let body_bb = cx.context.append_basic_block(function, "body");
        cx.builder.position_at_end(body_bb);
        let mut fcx = FnCx {
            cx,
            fns,
            function,
            mir_fn,
            locals: Vec::new(),
            frames: Vec::new(),
            entry,
        };
        // Local slots. Parameters copy their incoming values.
        for (i, &ty) in mir_fn.locals.iter().enumerate() {
            let slot = fcx.entry_alloca(cx.basic_ty(ty), &format!("local{i}"));
            fcx.locals.push((slot, ty));
            if i < mir_fn.param_count {
                let arg = function.get_nth_param(i as u32).expect("param");
                cx.builder.build_store(slot, arg).expect("store");
            } else {
                // Non-param locals get their type's default (SPECS §3.11)
                // so reads-before-writes are defined.
                let init: BasicValueEnum = match ty {
                    Ty::Int | Ty::UInt => cx.context.i32_type().const_zero().into(),
                    Ty::Number => cx.context.f64_type().const_float(f64::NAN).into(),
                    Ty::Boolean => cx.context.bool_type().const_zero().into(),
                    Ty::String => cx.ptr().const_null().into(),
                    Ty::Any => cx.any_ty.const_zero().into(), // tag 0 = undefined
                    Ty::Void => unreachable!(),
                };
                cx.builder.build_store(slot, init).expect("store");
            }
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
            Ty::String => b
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

    fn stmt(&mut self, stmt: &Stmt) {
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
                self.frames.push(Frame {
                    label: label.clone(),
                    break_bb: end_bb,
                    continue_bb: Some(cond_bb),
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
                self.frames.push(Frame {
                    label: label.clone(),
                    break_bb: end_bb,
                    continue_bb: Some(cond_bb),
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
                self.frames.push(Frame {
                    label: label.clone(),
                    break_bb: end_bb,
                    continue_bb: Some(update_bb),
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
                let target = self.find_frame(label.as_deref(), false);
                self.cx
                    .builder
                    .build_unconditional_branch(target)
                    .expect("br");
            }
            Stmt::Continue { label } => {
                let target = self.find_frame(label.as_deref(), true);
                self.cx
                    .builder
                    .build_unconditional_branch(target)
                    .expect("br");
            }
            Stmt::Return { value } => {
                match value {
                    Some(v) => {
                        let v = self.expr(v);
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
            Stmt::Empty => {}
        }
    }

    fn branch_if_open(&self, target: BasicBlock<'ctx>) {
        if self.current_block_open() {
            self.cx
                .builder
                .build_unconditional_branch(target)
                .expect("br");
        }
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
    fn switch(&mut self, scrutinee: &mir::Expr, cases: &[mir::Case]) {
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
        }
    }

    fn load_local(&mut self, id: mir::LocalId) -> Val<'ctx> {
        let (slot, ty) = self.locals[id.0 as usize];
        let cx = self.cx;
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
        let (slot, ty) = self.locals[id.0 as usize];
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
            Val::Str(p) => p.into(),
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
                        let name = if conv == Conv::ToInt {
                            "vs_f64_to_int32"
                        } else {
                            "vs_f64_to_uint32"
                        };
                        let rf = cx.runtime_fn(
                            name,
                            Some(cx.context.i32_type().into()),
                            &[cx.context.f64_type().into()],
                        );
                        let call = self
                            .cx
                            .builder
                            .build_call(rf, &[f.into()], "toi")
                            .expect("call");
                        wrap(
                            call.try_as_basic_value()
                                .basic()
                                .expect("value")
                                .into_int_value(),
                        )
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
        }
    }

    fn convert_num_to_int(&mut self, f: FloatValue<'ctx>, conv: Conv) -> Val<'ctx> {
        let cx = self.cx;
        let name = if conv == Conv::ToInt {
            "vs_f64_to_int32"
        } else {
            "vs_f64_to_uint32"
        };
        let rf = cx.runtime_fn(
            name,
            Some(cx.context.i32_type().into()),
            &[cx.context.f64_type().into()],
        );
        let call = self
            .cx
            .builder
            .build_call(rf, &[f.into()], "toi")
            .expect("call");
        let iv = call
            .try_as_basic_value()
            .basic()
            .expect("value")
            .into_int_value();
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
        let (name, arg_kinds, ret_str): (&str, &[ArgKind], bool) = match m {
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

enum ArgKind {
    Num,
    Str,
}

fn ty_tag(ty: Ty) -> u32 {
    match ty {
        Ty::Int => tag::INT,
        Ty::UInt => tag::UINT,
        Ty::Number => tag::NUMBER,
        Ty::Boolean => tag::BOOLEAN,
        Ty::String => tag::STRING,
        Ty::Any | Ty::Void => tag::NULL,
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
