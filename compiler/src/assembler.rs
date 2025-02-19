use itertools::Itertools;
use redscript::ast::{Constant, Expr, Ident, Intrinsic, Literal, Seq, Span, TypeName};
use redscript::bundle::{ConstantPool, PoolIndex};
use redscript::bytecode::{Code, Instr, Label, Location, Offset};
use redscript::definition::{Definition, Function, Local, Type};

use crate::error::{Cause, Error, ResultSpan};
use crate::scope::{Reference, Scope, TypeId, Value};
use crate::source_map::Files;
use crate::symbol::Symbol;
use crate::typechecker::{type_of, Callable, Member, TypedAst, TypedExpr, TypedExprExt};

pub struct Assembler<'a> {
    files: &'a Files,
    instructions: Vec<Instr<Label>>,
    labels: usize,
}

impl<'a> Assembler<'a> {
    fn new(files: &'a Files) -> Self {
        Self {
            files,
            instructions: Vec::new(),
            labels: 0,
        }
    }

    #[inline]
    fn emit(&mut self, instr: Instr<Label>) {
        self.instructions.push(instr);
    }

    #[inline]
    fn emit_label(&mut self, label: Label) {
        self.instructions.push(Instr::Target(label));
    }

    #[inline]
    fn new_label(&mut self) -> Label {
        let label = Label { index: self.labels };
        self.labels += 1;
        label
    }

    fn assemble(
        &mut self,
        expr: TypedExpr,
        scope: &mut Scope,
        pool: &mut ConstantPool,
        exit: Option<Label>,
    ) -> Result<(), Error> {
        match expr {
            Expr::Ident(reference, span) => {
                match reference {
                    Reference::Value(Value::Local(idx)) => self.emit(Instr::Local(idx)),
                    Reference::Value(Value::Parameter(idx)) => self.emit(Instr::Param(idx)),
                    _ => return Err(Cause::UnexpectedToken("symbol").with_span(span)),
                };
            }
            Expr::Constant(cons, _) => match cons {
                Constant::String(Literal::String, lit) => {
                    let idx = pool.strings.add(lit);
                    self.emit(Instr::StringConst(idx));
                }
                Constant::String(Literal::Name, lit) => {
                    let idx = pool.names.add(lit);
                    self.emit(Instr::NameConst(idx));
                }
                Constant::String(Literal::Resource, lit) => {
                    let idx = pool.resources.add(lit);
                    self.emit(Instr::ResourceConst(idx));
                }
                Constant::String(Literal::TweakDbId, lit) => {
                    let idx = pool.tweakdb_ids.add(lit);
                    self.emit(Instr::TweakDbIdConst(idx));
                }
                Constant::F32(val) => {
                    self.emit(Instr::F32Const(val));
                }
                Constant::F64(val) => {
                    self.emit(Instr::F64Const(val));
                }
                Constant::I32(val) => {
                    self.emit(Instr::I32Const(val));
                }
                Constant::I64(val) => {
                    self.emit(Instr::I64Const(val));
                }
                Constant::U32(val) => {
                    self.emit(Instr::U32Const(val));
                }
                Constant::U64(val) => {
                    self.emit(Instr::U64Const(val));
                }
                Constant::Bool(true) => {
                    self.emit(Instr::TrueConst);
                }
                Constant::Bool(false) => {
                    self.emit(Instr::FalseConst);
                }
            },
            Expr::Cast(type_, expr, span) => {
                if let TypeId::Class(class) = type_ {
                    self.emit(Instr::DynamicCast(class, 0));
                    self.assemble(*expr, scope, pool, None)?;
                } else {
                    return Err(Cause::UnsupportedOperation("casting", type_.pretty(pool)?).with_span(span));
                }
            }
            Expr::Declare(local, typ, init, span) => {
                if let Some(val) = init {
                    self.emit(Instr::Assign);
                    self.emit(Instr::Local(local));
                    self.assemble(*val, scope, pool, None)?;
                } else {
                    let typ = typ.expect("Local without type");
                    self.emit_initializer(local, *typ, scope, pool).with_span(span)?;
                }
            }
            Expr::Assign(lhs, rhs, _) => {
                self.emit(Instr::Assign);
                self.assemble(*lhs, scope, pool, None)?;
                self.assemble(*rhs, scope, pool, None)?;
            }
            Expr::ArrayElem(expr, idx, span) => {
                match type_of(&expr, scope, pool)? {
                    type_ @ TypeId::Array(_) => {
                        let type_idx = scope.get_type_index(&type_, pool).with_span(span)?;
                        self.emit(Instr::ArrayElement(type_idx));
                    }
                    type_ @ TypeId::StaticArray(_, _) => {
                        let type_idx = scope.get_type_index(&type_, pool).with_span(span)?;
                        self.emit(Instr::StaticArrayElement(type_idx));
                    }
                    other => return Err(Cause::UnsupportedOperation("indexing", other.pretty(pool)?).with_span(span)),
                }
                self.assemble(*expr, scope, pool, None)?;
                self.assemble(*idx, scope, pool, None)?;
            }
            Expr::New(type_, args, span) => match type_ {
                TypeId::Class(idx) => self.emit(Instr::New(idx)),
                TypeId::Struct(idx) => {
                    self.emit(Instr::Construct(args.len() as u8, idx));
                    for arg in args.into_vec() {
                        self.assemble(arg, scope, pool, None)?;
                    }
                }
                _ => return Err(Cause::UnsupportedOperation("constructing", type_.pretty(pool)?).with_span(span)),
            },
            Expr::Return(Some(expr), _) => {
                self.emit(Instr::Return);
                self.assemble(*expr, scope, pool, None)?;
            }
            Expr::Return(None, _) => {
                self.emit(Instr::Return);
                self.emit(Instr::Nop);
            }
            Expr::Seq(seq) => {
                self.assemble_seq(seq, scope, pool, exit)?;
            }
            Expr::Switch(expr, cases, default, span) => {
                let type_ = type_of(&expr, scope, pool)?;
                let first_case_label = self.new_label();
                let mut next_case_label = self.new_label();
                let exit_label = self.new_label();
                let type_idx = scope.get_type_index(&type_, pool).with_span(span)?;
                self.emit(Instr::Switch(type_idx, first_case_label));
                self.assemble(*expr, scope, pool, None)?;
                self.emit_label(first_case_label);

                let mut case_iter = cases.into_iter().peekable();
                while case_iter.peek().is_some() {
                    let body_label = self.new_label();

                    for case in &mut case_iter {
                        self.emit_label(next_case_label);
                        next_case_label = self.new_label();
                        self.emit(Instr::SwitchLabel(next_case_label, body_label));
                        self.assemble(case.matcher, scope, pool, None)?;

                        if !case.body.exprs.iter().all(Expr::is_empty) {
                            self.emit_label(body_label);
                            self.assemble_seq(case.body, scope, pool, Some(exit_label))?;
                            break;
                        }
                    }
                }
                self.emit_label(next_case_label);

                if let Some(body) = default {
                    self.emit(Instr::SwitchDefault);
                    self.assemble_seq(body, scope, pool, Some(exit_label))?;
                }
                self.emit_label(exit_label);
            }
            Expr::If(condition, if_, else_, _) => {
                let else_label = self.new_label();
                self.emit(Instr::JumpIfFalse(else_label));
                self.assemble(*condition, scope, pool, None)?;
                self.assemble_seq(if_, scope, pool, exit)?;
                if let Some(else_code) = else_ {
                    let exit_label = self.new_label();
                    self.emit(Instr::Jump(exit_label));
                    self.emit_label(else_label);
                    self.assemble_seq(else_code, scope, pool, exit)?;
                    self.emit_label(exit_label);
                } else {
                    self.emit_label(else_label);
                }
            }
            Expr::Conditional(cond, true_, false_, _) => {
                let false_label = self.new_label();
                let exit_label = self.new_label();
                self.emit(Instr::Conditional(false_label, exit_label));
                self.assemble(*cond, scope, pool, None)?;
                self.assemble(*true_, scope, pool, None)?;
                self.emit_label(false_label);
                self.assemble(*false_, scope, pool, None)?;
                self.emit_label(exit_label);
            }
            Expr::While(cond, body, _) => {
                let exit_label = self.new_label();
                let loop_label = self.new_label();
                self.emit_label(loop_label);
                self.emit(Instr::JumpIfFalse(exit_label));
                self.assemble(*cond, scope, pool, None)?;
                self.assemble_seq(body, scope, pool, Some(exit_label))?;
                self.emit(Instr::Jump(loop_label));
                self.emit_label(exit_label);
            }
            Expr::Member(expr, member, _) => match member {
                Member::ClassField(field) => {
                    let exit_label = self.new_label();
                    self.emit(Instr::Context(exit_label));
                    self.assemble(*expr, scope, pool, None)?;
                    self.emit(Instr::ObjectField(field));
                    self.emit_label(exit_label);
                }
                Member::StructField(field) => {
                    self.emit(Instr::StructField(field));
                    self.assemble(*expr, scope, pool, None)?;
                }
                Member::EnumMember(enum_, member) => {
                    self.emit(Instr::EnumConst(enum_, member));
                }
            },
            Expr::Call(callable, _, args, span) => match callable {
                Callable::Function(fun) => {
                    self.assemble_call(fun, args.into_vec(), scope, pool, false, span)?;
                }
                Callable::Intrinsic(op, type_) => {
                    self.assemble_intrinsic(op, args.into_vec(), &type_, scope, pool, span)?;
                }
            },
            Expr::MethodCall(expr, fun_idx, args, span) => match *expr {
                Expr::Ident(Reference::Symbol(Symbol::Class(_, _) | Symbol::Struct(_, _)), span) => {
                    self.assemble_call(fun_idx, args, scope, pool, true, span)?;
                }
                expr => {
                    let force_static_call = matches!(&expr, Expr::Super(_));
                    let exit_label = self.new_label();
                    self.emit(Instr::Context(exit_label));
                    self.assemble(expr, scope, pool, None)?;
                    self.assemble_call(fun_idx, args, scope, pool, force_static_call, span)?;
                    self.emit_label(exit_label);
                }
            },

            Expr::Null(_) => {
                self.emit(Instr::Null);
            }
            Expr::This(_) | Expr::Super(_) => {
                self.emit(Instr::This);
            }
            Expr::Break(_) if exit.is_some() => {
                self.emit(Instr::Jump(exit.unwrap()));
            }
            Expr::ArrayLit(_, _, span) => return Err(Cause::UnsupportedFeature("ArrayLit").with_span(span)),
            Expr::InterpolatedString(_, _, span) => {
                return Err(Cause::UnsupportedFeature("InterpolatedString").with_span(span))
            }
            Expr::ForIn(_, _, _, span) => return Err(Cause::UnsupportedFeature("For-in").with_span(span)),
            Expr::BinOp(_, _, _, span) => return Err(Cause::UnsupportedFeature("BinOp").with_span(span)),
            Expr::UnOp(_, _, span) => return Err(Cause::UnsupportedFeature("UnOp").with_span(span)),
            Expr::Break(span) => return Err(Cause::UnsupportedFeature("Break").with_span(span)),
            Expr::Goto(_, span) => return Err(Cause::UnsupportedFeature("Goto").with_span(span)),
        };
        Ok(())
    }

    fn assemble_seq(
        &mut self,
        seq: Seq<TypedAst>,
        scope: &mut Scope,
        pool: &mut ConstantPool,
        exit: Option<Label>,
    ) -> Result<(), Error> {
        for expr in seq.exprs {
            self.assemble(expr, scope, pool, exit)?;
        }
        Ok(())
    }

    fn emit_initializer(
        &mut self,
        local: PoolIndex<Local>,
        typ: TypeId,
        scope: &mut Scope,
        pool: &mut ConstantPool,
    ) -> Result<(), Cause> {
        fn get_initializer(typ: &TypeId, pool: &mut ConstantPool) -> Result<Option<Instr<Label>>, Cause> {
            let res = match typ {
                &TypeId::Prim(typ_idx) => match Ident::from_heap(pool.def_name(typ_idx)?) {
                    tp if tp == TypeName::BOOL.name() => Some(Instr::FalseConst),
                    tp if tp == TypeName::INT8.name() => Some(Instr::I8Const(0)),
                    tp if tp == TypeName::INT16.name() => Some(Instr::I16Const(0)),
                    tp if tp == TypeName::INT32.name() => Some(Instr::I32Zero),
                    tp if tp == TypeName::INT64.name() => Some(Instr::I64Const(0)),
                    tp if tp == TypeName::UINT8.name() => Some(Instr::U8Const(0)),
                    tp if tp == TypeName::UINT16.name() => Some(Instr::U16Const(0)),
                    tp if tp == TypeName::UINT32.name() => Some(Instr::U32Const(0)),
                    tp if tp == TypeName::UINT64.name() => Some(Instr::U64Const(0)),
                    tp if tp == TypeName::FLOAT.name() => Some(Instr::F32Const(0.0)),
                    tp if tp == TypeName::DOUBLE.name() => Some(Instr::F64Const(0.0)),
                    tp if tp == TypeName::STRING.name() => {
                        let empty = pool.strings.add("".into());
                        Some(Instr::StringConst(empty))
                    }
                    tp if tp == TypeName::CNAME.name() => Some(Instr::NameConst(PoolIndex::UNDEFINED)),
                    tp if tp == TypeName::TWEAKDB_ID.name() => Some(Instr::TweakDbIdConst(PoolIndex::UNDEFINED)),
                    tp if tp == TypeName::RESOURCE.name() => Some(Instr::ResourceConst(PoolIndex::UNDEFINED)),
                    _ => None,
                },
                &TypeId::Struct(struct_) => Some(Instr::Construct(0, struct_)),
                &TypeId::Enum(enum_idx) => {
                    let enum_ = pool.enum_(enum_idx)?;
                    enum_.members.first().map(|member| Instr::EnumConst(enum_idx, *member))
                }
                TypeId::Ref(_) => Some(Instr::Null),
                TypeId::WeakRef(_) => Some(Instr::WeakRefNull),
                TypeId::Array(_) | TypeId::StaticArray(_, _) => {
                    return Err(Cause::UnsupportedFeature(
                        "initializing a static array with another array",
                    ));
                }
                _ => None,
            };
            Ok(res)
        }

        match &typ {
            TypeId::Array(_) => {
                self.emit(Instr::ArrayClear(scope.get_type_index(&typ, pool)?));
                self.emit(Instr::Local(local));
            }
            TypeId::StaticArray(elem, size) => {
                if let Some(instr) = get_initializer(elem, pool)? {
                    let type_idx = scope.get_type_index(&typ, pool)?;
                    for i in 0..*size {
                        self.emit(Instr::Assign);
                        self.emit(Instr::StaticArrayElement(type_idx));
                        self.emit(Instr::Local(local));
                        self.emit(Instr::U32Const(i));
                        self.emit(instr.clone());
                    }
                }
            }
            _ => {
                if let Some(instr) = get_initializer(&typ, pool)? {
                    self.emit(Instr::Assign);
                    self.emit(Instr::Local(local));
                    self.emit(instr);
                }
            }
        }

        Ok(())
    }

    fn assemble_call(
        &mut self,
        function_idx: PoolIndex<Function>,
        args: Vec<TypedExpr>,
        scope: &mut Scope,
        pool: &mut ConstantPool,
        force_static: bool,
        span: Span,
    ) -> Result<(), Error> {
        let fun = pool.function(function_idx)?;
        let fun_flags = fun.flags;
        let param_flags: Vec<_> = fun
            .parameters
            .iter()
            .map(|idx| pool.parameter(*idx).map(|param| param.flags))
            .try_collect()?;
        let args_len = args.len();
        let exit_label = self.new_label();
        let mut invoke_flags = 0u16;
        for (n, arg) in args.iter().enumerate() {
            let is_rvalue_ref = Self::is_rvalue_ref(arg, scope, pool).unwrap_or(false);
            if is_rvalue_ref {
                invoke_flags |= 1 << n;
            }
        }

        let line = self
            .files
            .lookup(span)
            .and_then(|loc| loc.start.line.try_into().ok())
            .unwrap_or_default();
        if !force_static && !fun_flags.is_final() && !fun_flags.is_static() && !fun_flags.is_native() {
            let name_idx = pool.definition(function_idx)?.name;
            self.emit(Instr::InvokeVirtual(exit_label, line, name_idx, invoke_flags));
        } else {
            self.emit(Instr::InvokeStatic(exit_label, line, function_idx, invoke_flags));
        }
        for (arg, flags) in args.into_iter().zip(&param_flags) {
            if flags.is_short_circuit() {
                let skip_label = self.new_label();
                self.emit(Instr::Skip(skip_label));
                self.assemble(arg, scope, pool, None)?;
                self.emit_label(skip_label);
            } else {
                self.assemble(arg, scope, pool, None)?;
            }
        }
        if param_flags.len() < args_len {
            return Err(Error::CompileError(
                Cause::UnsupportedFeature("cannot emit function call (probably invalid signature)"),
                Span::ZERO,
            ));
        }
        for _ in 0..param_flags.len() - args_len {
            self.emit(Instr::Nop);
        }
        self.emit(Instr::ParamEnd);
        self.emit_label(exit_label);
        Ok(())
    }

    fn is_rvalue_ref(expr: &TypedExpr, scope: &Scope, pool: &ConstantPool) -> Option<bool> {
        let typ = type_of(expr, scope, pool).ok()?;
        match typ {
            TypeId::ScriptRef(_) => match expr {
                Expr::Call(Callable::Intrinsic(Intrinsic::AsRef, _), _, args, _) => match args.first() {
                    Some(expr) => Some(expr.is_prvalue()),
                    _ => Some(true),
                },
                _ => Some(true),
            },
            _ => None,
        }
    }

    fn assemble_intrinsic(
        &mut self,
        intrinsic: Intrinsic,
        args: Vec<TypedExpr>,
        return_type: &TypeId,
        scope: &mut Scope,
        pool: &mut ConstantPool,
        span: Span,
    ) -> Result<(), Error> {
        let mut get_arg_type =
            |i| type_of(&args[i], scope, pool).and_then(|typ| scope.get_type_index(&typ, pool).with_span(span));

        match intrinsic {
            Intrinsic::Equals => {
                // TODO: eventually enforce type compatibility (https://github.com/jac3km4/redscript/issues/69)
                self.emit(Instr::Equals(get_arg_type(0)?));
            }
            Intrinsic::NotEquals => {
                // TODO: eventually enforce type compatibility (https://github.com/jac3km4/redscript/issues/69)
                self.emit(Instr::NotEquals(get_arg_type(0)?));
            }
            Intrinsic::ArrayClear => {
                self.emit(Instr::ArrayClear(get_arg_type(0)?));
            }
            Intrinsic::ArraySize => {
                let idx = get_arg_type(0)?;
                if matches!(pool.type_(idx)?, Type::StaticArray(_, _)) {
                    self.emit(Instr::StaticArraySize(idx));
                } else {
                    self.emit(Instr::ArraySize(idx));
                }
            }
            Intrinsic::ArrayResize => {
                self.emit(Instr::ArrayResize(get_arg_type(0)?));
            }
            Intrinsic::ArrayFindFirst => {
                let idx = get_arg_type(0)?;
                if matches!(pool.type_(idx)?, Type::StaticArray(_, _)) {
                    self.emit(Instr::StaticArrayFindFirst(idx));
                } else {
                    self.emit(Instr::ArrayFindFirst(idx));
                }
            }
            Intrinsic::ArrayFindLast => {
                let idx = get_arg_type(0)?;
                if matches!(pool.type_(idx)?, Type::StaticArray(_, _)) {
                    self.emit(Instr::StaticArrayFindLast(idx));
                } else {
                    self.emit(Instr::ArrayFindLast(idx));
                }
            }
            Intrinsic::ArrayContains => {
                let idx = get_arg_type(0)?;
                if matches!(pool.type_(idx)?, Type::StaticArray(_, _)) {
                    self.emit(Instr::StaticArrayContains(idx));
                } else {
                    self.emit(Instr::ArrayContains(idx));
                }
            }
            Intrinsic::ArrayCount => {
                let idx = get_arg_type(0)?;
                if matches!(pool.type_(idx)?, Type::StaticArray(_, _)) {
                    self.emit(Instr::StaticArrayCount(idx));
                } else {
                    self.emit(Instr::ArrayCount(idx));
                }
            }
            Intrinsic::ArrayPush => {
                self.emit(Instr::ArrayPush(get_arg_type(0)?));
            }
            Intrinsic::ArrayPop => {
                self.emit(Instr::ArrayPop(get_arg_type(0)?));
            }
            Intrinsic::ArrayInsert => {
                self.emit(Instr::ArrayInsert(get_arg_type(0)?));
            }
            Intrinsic::ArrayRemove => {
                self.emit(Instr::ArrayRemove(get_arg_type(0)?));
            }
            Intrinsic::ArrayGrow => {
                self.emit(Instr::ArrayGrow(get_arg_type(0)?));
            }
            Intrinsic::ArrayErase => {
                self.emit(Instr::ArrayErase(get_arg_type(0)?));
            }
            Intrinsic::ArrayLast => {
                self.emit(Instr::ArrayLast(get_arg_type(0)?));
            }
            Intrinsic::ArraySort => {
                self.emit(Instr::ArraySort(get_arg_type(0)?));
            }
            Intrinsic::ArraySortByPredicate => {
                self.emit(Instr::ArraySortByPredicate(get_arg_type(0)?));
            }
            Intrinsic::ToString => match type_of(&args[0], scope, pool)? {
                TypeId::Variant => self.emit(Instr::VariantToString),
                any => {
                    let type_idx = scope.get_type_index(&any, pool).with_span(span)?;
                    self.emit(Instr::ToString(type_idx));
                }
            },
            Intrinsic::EnumInt => {
                self.emit(Instr::EnumToI32(get_arg_type(0)?, 4));
            }
            Intrinsic::IntEnum => {
                let type_idx = scope.get_type_index(return_type, pool).with_span(span)?;
                self.emit(Instr::I32ToEnum(type_idx, 4));
            }
            Intrinsic::ToVariant => {
                self.emit(Instr::ToVariant(get_arg_type(0)?));
            }
            Intrinsic::FromVariant => {
                let type_idx = scope.get_type_index(return_type, pool).with_span(span)?;
                self.emit(Instr::FromVariant(type_idx));
            }
            Intrinsic::VariantIsRef => {
                self.emit(Instr::VariantIsRef);
            }
            Intrinsic::VariantIsArray => {
                self.emit(Instr::VariantIsArray);
            }
            Intrinsic::VariantTypeName => {
                self.emit(Instr::VariantTypeName);
            }
            Intrinsic::AsRef => {
                self.emit(Instr::AsRef(get_arg_type(0)?));
            }
            Intrinsic::Deref => {
                let type_idx = scope.get_type_index(return_type, pool).with_span(span)?;
                self.emit(Instr::Deref(type_idx));
            }
            Intrinsic::RefToWeakRef => {
                self.emit(Instr::RefToWeakRef);
            }
            Intrinsic::WeakRefToRef => {
                self.emit(Instr::WeakRefToRef);
            }
            Intrinsic::IsDefined => match type_of(&args[0], scope, pool)? {
                TypeId::Ref(_) | TypeId::Null => self.emit(Instr::RefToBool),
                TypeId::WeakRef(_) => self.emit(Instr::WeakRefToBool),
                TypeId::Variant => self.emit(Instr::VariantIsDefined),
                _ => panic!("Invalid IsDefined parameter"),
            },
            Intrinsic::NameOf => {
                let idx: PoolIndex<Definition> = match type_of(&args[0], scope, pool)? {
                    TypeId::Enum(idx) => idx.cast(),
                    TypeId::Class(idx) | TypeId::Struct(idx) => idx.cast(),
                    _ => panic!("Invalid NameOf parameter"),
                };
                self.emit(Instr::NameConst(pool.definition(idx)?.name));
                return Ok(());
            }
        };
        for arg in args {
            self.assemble(arg, scope, pool, None)?;
        }
        Ok(())
    }

    fn into_code(self) -> Code<Offset> {
        let mut locations = Vec::with_capacity(self.labels);
        locations.resize(self.labels, Location::new(0));

        let code = Code::new(self.instructions);
        for (loc, instr) in code.iter() {
            if let Instr::Target(label) = instr {
                locations[label.index] = loc;
            }
        }

        let mut resolved = Vec::with_capacity(code.len());
        for (loc, instr) in code.iter().filter(|(_, instr)| !matches!(instr, Instr::Target(_))) {
            resolved.push(instr.resolve_labels(loc, &locations));
        }
        Code::new(resolved)
    }

    pub fn from_body(
        seq: Seq<TypedAst>,
        files: &'a Files,
        scope: &mut Scope,
        pool: &mut ConstantPool,
    ) -> Result<Code<Offset>, Error> {
        let mut assembler = Self::new(files);
        assembler.assemble_seq(seq, scope, pool, None)?;
        assembler.emit(Instr::Nop);
        Ok(assembler.into_code())
    }
}
