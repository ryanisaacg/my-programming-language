use std::collections::HashMap;

use crate::{
    analyzer::{
        BinOpComparison, BinOpNumeric, FunDecl, IRContext, IRExpression, IRExpressionValue,
        IRStatement, IRStatementValue, IRType, NumericType,
    },
    tree::{Node, NodePtr},
};
use wasm_encoder::*;

/**
 * Note: currently in WASM, there is only a 0-memory. However, the spec is forwards-compatible with
 * more
 */
const MAIN_MEMORY: u32 = 0;
const STACK_MINIMUM_PAGES: u64 = 16;
const HEAP_MINIMUM_PAGES: u64 = 48;
const MEMORY_MINIMUM_PAGES: u64 = STACK_MINIMUM_PAGES + HEAP_MINIMUM_PAGES;
const MAXIMUM_MEMORY: u64 = 16384;

// TODO: handle stack overflows
const STACK_PTR: u32 = 0;
const BASE_PTR: u32 = 1;
const ASSIGNMENT_PTR: u32 = 2;

pub fn emit(statements: Vec<IRStatement>, arena: &IRContext) -> Vec<u8> {
    let mut module = Module::new();
    let mut types = TypeSection::new();
    let mut functions = FunctionSection::new();
    let mut exports = ExportSection::new();
    let mut codes = CodeSection::new();
    let mut globals = GlobalSection::new();

    let mut function_indices = HashMap::new();

    // TODO: how stable is this order

    let mut current_function_idx = 0;
    for statement in statements.iter() {
        if let IRStatementValue::FunctionDeclaration(decl) = &statement.value {
            function_indices.insert(decl.name.clone(), current_function_idx);
            current_function_idx += 1;
        }
    }

    // Set up the stack pointer
    globals.global(
        GlobalType {
            val_type: ValType::I32,
            mutable: true,
        },
        &ConstExpr::i32_const(4),
    );
    // Base pointer
    globals.global(
        GlobalType {
            val_type: ValType::I32,
            mutable: true,
        },
        &ConstExpr::i32_const(4),
    );
    // Assignment pointer
    globals.global(
        GlobalType {
            val_type: ValType::I32,
            mutable: true,
        },
        &ConstExpr::i32_const(0x50),
    );

    let type_representations = arena
        .kinds()
        .enumerate()
        .map(|(idx, _)| represented_by(arena, idx))
        .collect::<Vec<_>>();

    current_function_idx = 0;

    for statement in statements {
        if let IRStatementValue::FunctionDeclaration(decl) = statement.value {
            emit_function_types(&decl, &type_representations, &mut types);
            functions.function(current_function_idx);
            // TODO: should we export function
            exports.export(decl.name.as_ref(), ExportKind::Func, current_function_idx);
            let LocalsAnalysis {
                name_to_offset,
                parameter_locals,
                stack_size,
            } = analyze_locals(&decl, &type_representations, arena);
            let param_count = parameter_locals.len();
            let mut ctx = EmitContext {
                instructions: Vec::new(),
                arena,
                stack_offsets: &name_to_offset,
                functions: &function_indices,
                representations: &type_representations,
                wasm_locals: parameter_locals,
            };
            emit_function_declaration(&mut ctx, stack_size, decl);
            let mut wasm_locals = Vec::new();
            // TODO: optimize by combining adjacent locals
            // TODO: is there a bug in wasm-encode?
            for val_type in ctx.wasm_locals.drain(param_count..) {
                wasm_locals.push((1, val_type));
            }
            let mut f = Function::new(wasm_locals);
            for instruction in ctx.instructions.iter() {
                f.instruction(instruction);
            }
            codes.function(&f);

            current_function_idx += 1;
        }
    }

    module.section(&types);
    module.section(&functions);
    let mut memories = MemorySection::new();
    memories.memory(MemoryType {
        minimum: MEMORY_MINIMUM_PAGES,
        maximum: Some(MAXIMUM_MEMORY),
        memory64: false,
        shared: false,
    });
    exports.export("memory", ExportKind::Memory, 0);
    module.section(&memories);
    module.section(&globals);
    module.section(&exports);
    module.section(&codes);

    module.finish()
}

fn emit_function_types(
    decl: &FunDecl,
    type_representations: &Vec<Representation>,
    types: &mut TypeSection,
) {
    let mut params = Vec::new();
    for param in decl.params.iter() {
        flatten_repr(&type_representations[param.kind], &mut params);
    }
    let mut results = Vec::new();
    flatten_repr(&type_representations[decl.returns], &mut results);

    types.function(params, results);
}

// TODO: handle different bind points with the same name

struct LocalsAnalysis {
    name_to_offset: HashMap<String, u32>,
    parameter_locals: Vec<ValType>,
    stack_size: u32,
}

fn analyze_locals(
    decl: &FunDecl,
    type_representations: &Vec<Representation>,
    arena: &IRContext,
) -> LocalsAnalysis {
    let mut results = LocalsAnalysis {
        name_to_offset: HashMap::new(),
        parameter_locals: Vec::new(),
        stack_size: 4,
    };

    let mut repr_buffer = Vec::new();

    for param in decl.params.iter() {
        results
            .name_to_offset
            .insert(param.name.to_string(), results.stack_size);
        let repr = &type_representations[param.kind];
        results.stack_size += size_of_repr(repr);
        flatten_repr(repr, &mut repr_buffer);

        for val_type in repr_buffer.drain(..) {
            results.parameter_locals.push(val_type);
        }
    }

    for node in arena.iter_from(NodePtr::Expression(decl.body)) {
        if let Node::Statement(IRStatement {
            value: IRStatementValue::Declaration(name, expr),
            ..
        }) = node
        {
            let expr = arena.expression(*expr);
            results
                .name_to_offset
                .insert(name.to_string(), results.stack_size);
            results.stack_size += size_of_repr(&type_representations[expr.kind]);
        }
    }

    results
}

struct EmitContext<'a> {
    instructions: Vec<Instruction<'a>>,
    arena: &'a IRContext,
    stack_offsets: &'a HashMap<String, u32>,
    functions: &'a HashMap<String, u32>,
    /**
     * A mapping from the indices of a type in the IRContext to that type's calculated
     * representation
     */
    representations: &'a Vec<Representation>,
    wasm_locals: Vec<ValType>,
}

impl<'a> EmitContext<'a> {
    fn add_instruction(&mut self, instruction: Instruction<'a>) {
        self.instructions.push(instruction);
    }
}

// TODO: consider dynamically-sized stack frames?

fn emit_function_declaration(ctx: &mut EmitContext<'_>, stack_size: u32, decl: FunDecl) {
    // Write the current base pointer to the base of the stack
    ctx.add_instruction(Instruction::GlobalGet(STACK_PTR));
    ctx.add_instruction(Instruction::GlobalGet(BASE_PTR));
    ctx.add_instruction(Instruction::I32Store(MemArg {
        offset: 0,
        align: 1,
        memory_index: MAIN_MEMORY,
    }));
    // Set the base pointer to the stack pointer
    ctx.add_instruction(Instruction::GlobalGet(STACK_PTR));
    ctx.add_instruction(Instruction::GlobalSet(BASE_PTR));
    // Set the new stack size
    ctx.add_instruction(Instruction::GlobalGet(STACK_PTR));
    ctx.add_instruction(Instruction::I32Const(stack_size as i32));
    ctx.add_instruction(Instruction::I32Add);
    // TODO: check for stack overflow
    ctx.add_instruction(Instruction::GlobalSet(STACK_PTR));
    let mut local_index = 0;
    for param in decl.params.iter() {
        let Some(offset) = ctx.stack_offsets.get(param.name.as_str())  else {
            panic!("internal compiler error: unknown name {}", param.name);
        };
        local_index = emit_parameter(
            &mut ctx.instructions,
            local_index,
            *offset,
            &ctx.representations[param.kind],
        );
    }
    emit_expression(ctx, ctx.arena.expression(decl.body));
    // Set the stack pointer back to the base pointer
    ctx.add_instruction(Instruction::GlobalGet(BASE_PTR));
    ctx.add_instruction(Instruction::GlobalSet(STACK_PTR));
    // Reset the base pointer
    ctx.add_instruction(Instruction::GlobalGet(BASE_PTR));
    ctx.add_instruction(Instruction::I32Load(MemArg {
        offset: 0,
        align: 1,
        memory_index: MAIN_MEMORY,
    }));
    ctx.add_instruction(Instruction::GlobalSet(BASE_PTR));
    ctx.add_instruction(Instruction::End);
}

fn emit_statement<'a>(ctx: &mut EmitContext<'a>, statement: &IRStatement) {
    match &statement.value {
        IRStatementValue::FunctionDeclaration(_) => {
            unreachable!(); // TODO
        }
        IRStatementValue::Expression(expr) => {
            let expr = ctx.arena.expression(*expr);
            emit_expression(ctx, expr);
        }
        IRStatementValue::Declaration(name, expr) => {
            // TODO: should this just be an expect
            let Some(offset) = ctx.stack_offsets.get(name.as_str())  else {
                panic!("internal compiler error: unknown name {}", name);
            };
            let expr = ctx.arena.expression(*expr);
            emit_expression(ctx, expr);
            ctx.add_instruction(Instruction::GlobalGet(BASE_PTR));
            ctx.add_instruction(Instruction::GlobalSet(ASSIGNMENT_PTR));
            emit_assignment(
                &mut ctx.instructions,
                &mut ctx.wasm_locals,
                *offset,
                &ctx.representations[expr.kind],
            );
        }
    }
}

fn emit_expression<'a>(ctx: &mut EmitContext<'a>, expr: &IRExpression) {
    match &expr.value {
        IRExpressionValue::StructLiteral(..) | IRExpressionValue::Dot(..) => todo!(),
        IRExpressionValue::Bool(val) => {
            if *val {
                ctx.add_instruction(Instruction::I32Const(1));
            } else {
                ctx.add_instruction(Instruction::I32Const(0));
            }
        }
        IRExpressionValue::Int(constant) => {
            ctx.add_instruction(Instruction::I64Const(*constant));
        }
        IRExpressionValue::Float(constant) => {
            ctx.add_instruction(Instruction::F64Const(*constant));
        }
        IRExpressionValue::Assignment(lvalue, expr) => {
            let expr = ctx.arena.expression(*expr);
            emit_expression(ctx, expr);
            let offset = emit_lvalue(ctx, *lvalue);
            ctx.add_instruction(Instruction::GlobalSet(ASSIGNMENT_PTR));
            emit_assignment(
                &mut ctx.instructions,
                &mut ctx.wasm_locals,
                offset,
                &ctx.representations[expr.kind],
            );
        }
        IRExpressionValue::Dereference(child) => {
            emit_expression(ctx, ctx.arena.expression(*child));
            emit_dereference(&mut ctx.instructions, 0, &ctx.representations[expr.kind]);
        }
        IRExpressionValue::Call(function, arguments) => {
            for arg in arguments.iter() {
                let arg = ctx.arena.expression(*arg);
                emit_expression(ctx, arg);
            }
            let function = ctx.arena.expression(*function);
            // TODO: be able to emit other functions?
            let called_function = match &function.value {
                IRExpressionValue::LocalVariable(name) => name,
                _ => todo!(),
            };
            let function_index = ctx.functions.get(called_function).unwrap();
            ctx.add_instruction(Instruction::Call(*function_index));
        }
        IRExpressionValue::TakeShared(child) | IRExpressionValue::TakeUnique(child) => {
            let child = ctx.arena.expression(*child);
            // TODO: support non-variable references?
            match &child.value {
                IRExpressionValue::LocalVariable(name) => {
                    ctx.add_instruction(Instruction::GlobalGet(BASE_PTR));
                    let offset = ctx.stack_offsets.get(name.as_str()).unwrap();
                    ctx.add_instruction(Instruction::I32Const(*offset as i32));
                    ctx.add_instruction(Instruction::I32Add);
                }
                _ => todo!(),
            }
        }
        IRExpressionValue::LocalVariable(name) => {
            ctx.add_instruction(Instruction::GlobalGet(BASE_PTR));
            emit_dereference(
                &mut ctx.instructions,
                *ctx.stack_offsets.get(name.as_str()).unwrap(),
                &ctx.representations[expr.kind],
            );
        }
        IRExpressionValue::BinaryNumeric(operator, left, right) => {
            emit_expression(ctx, ctx.arena.expression(*left));
            emit_expression(ctx, ctx.arena.expression(*right));
            match operator {
                BinOpNumeric::Add => {
                    ctx.add_instruction(match ctx.arena.kind(expr.kind) {
                        IRType::Number(NumericType::Int64) => Instruction::I64Add,
                        IRType::Number(NumericType::Float64) => Instruction::F64Add,
                        _ => unreachable!(),
                    });
                }
                BinOpNumeric::Subtract => {
                    ctx.add_instruction(match ctx.arena.kind(expr.kind) {
                        IRType::Number(NumericType::Int64) => Instruction::I64Sub,
                        IRType::Number(NumericType::Float64) => Instruction::F64Sub,
                        _ => unreachable!(),
                    });
                }
            }
        }
        IRExpressionValue::Comparison(operator, left, right) => {
            let left = ctx.arena.expression(*left);
            emit_expression(ctx, left);
            emit_expression(ctx, ctx.arena.expression(*right));
            match operator {
                BinOpComparison::GreaterThan => {
                    ctx.add_instruction(match ctx.arena.kind(left.kind) {
                        IRType::Number(NumericType::Int64) => Instruction::I64GtS,
                        IRType::Number(NumericType::Float64) => Instruction::F64Gt,
                        _ => unreachable!(),
                    });
                }
                BinOpComparison::LessThan => {
                    ctx.add_instruction(match ctx.arena.kind(left.kind) {
                        IRType::Number(NumericType::Int64) => Instruction::I64LtS,
                        IRType::Number(NumericType::Float64) => Instruction::F64Lt,
                        _ => unreachable!(),
                    });
                }
            }
        }
        IRExpressionValue::If(predicate, block) => {
            emit_expression(ctx, ctx.arena.expression(*predicate));
            ctx.add_instruction(Instruction::If(BlockType::Empty)); // TODO
            emit_expression(ctx, ctx.arena.expression(*block));
            ctx.add_instruction(Instruction::End); // TODO
        }
        IRExpressionValue::While(predicate, block) => {
            ctx.add_instruction(Instruction::Loop(BlockType::Empty));
            emit_expression(ctx, ctx.arena.expression(*predicate));
            ctx.add_instruction(Instruction::If(BlockType::Empty)); // TODO
            emit_expression(ctx, ctx.arena.expression(*block));
            ctx.add_instruction(Instruction::Br(1)); // TODO: does this work
            ctx.add_instruction(Instruction::End); // TODO
            ctx.add_instruction(Instruction::End);
        }
        IRExpressionValue::Block(statements) => {
            for statement in statements {
                emit_statement(ctx, ctx.arena.statement(*statement));
            }
        }
    }
}

fn emit_parameter(
    instructions: &mut Vec<Instruction<'_>>,
    local_index: u32,
    offset: u32,
    representation: &Representation,
) -> u32 {
    match representation {
        Representation::Void => local_index,
        Representation::Scalar(scalar) => {
            instructions.push(Instruction::GlobalGet(BASE_PTR));
            instructions.push(Instruction::LocalGet(local_index as u32));
            // TODO: alignment
            let mem_arg = MemArg {
                offset: offset.into(),
                align: 1,
                memory_index: MAIN_MEMORY,
            };
            match scalar {
                ValType::I32 => {
                    instructions.push(Instruction::I32Store(mem_arg));
                }
                ValType::I64 => {
                    instructions.push(Instruction::I64Store(mem_arg));
                }
                ValType::F32 => {
                    instructions.push(Instruction::F32Store(mem_arg));
                }
                ValType::F64 => {
                    instructions.push(Instruction::F64Store(mem_arg));
                }
                _ => todo!(),
            }

            local_index + 1
        }
        Representation::Vector(reprs) => {
            let mut local_index = local_index;
            let mut offset = offset;
            for repr in reprs.iter() {
                local_index = emit_parameter(instructions, local_index, offset, repr);
                offset += size_of_repr(repr);
            }

            local_index
        }
    }
}

// TODO: do not spam this many locals
fn emit_assignment(
    instructions: &mut Vec<Instruction<'_>>,
    locals: &mut Vec<ValType>,
    offset: u32,
    representation: &Representation,
) {
    use Representation::*;

    match representation {
        Void => todo!(),
        Scalar(scalar) => {
            // TODO: unnecessary overhead for scalar assignments
            // TODO: alignment
            let mem_arg = MemArg {
                offset: offset.into(),
                align: 1,
                memory_index: MAIN_MEMORY,
            };
            let local_idx = locals.len() as u32;
            instructions.push(Instruction::LocalSet(local_idx));
            instructions.push(Instruction::GlobalGet(ASSIGNMENT_PTR));
            instructions.push(Instruction::LocalGet(local_idx));
            locals.push(*scalar);
            match scalar {
                ValType::I32 => {
                    instructions.push(Instruction::I32Store(mem_arg));
                }
                ValType::I64 => {
                    instructions.push(Instruction::I64Store(mem_arg));
                }
                ValType::F32 => {
                    instructions.push(Instruction::F32Store(mem_arg));
                }
                ValType::F64 => {
                    instructions.push(Instruction::F64Store(mem_arg));
                }
                _ => todo!("non-simple scalar assignments"),
            }
        }
        Vector(reprs) => {
            let mut offset = offset;
            for repr in reprs.iter() {
                emit_assignment(instructions, locals, offset, repr);
                offset += size_of_repr(repr);
            }
        }
    }
}

fn emit_dereference(
    instructions: &mut Vec<Instruction<'_>>,
    offset: u32,
    representation: &Representation,
) {
    use Representation::*;

    match representation {
        Void => todo!(),
        Scalar(scalar) => {
            // TODO: alignment
            let mem_arg = MemArg {
                offset: offset.into(),
                align: 1,
                memory_index: MAIN_MEMORY,
            };
            match scalar {
                ValType::I32 => {
                    instructions.push(Instruction::I32Load(mem_arg));
                }
                ValType::I64 => {
                    instructions.push(Instruction::I64Load(mem_arg));
                }
                ValType::F32 => {
                    instructions.push(Instruction::F32Load(mem_arg));
                }
                ValType::F64 => {
                    instructions.push(Instruction::F64Load(mem_arg));
                }
                _ => todo!("non-simple scalar assignments"),
            }
        }
        Vector(reprs) => {
            let mut offset = offset;
            for repr in reprs.iter() {
                emit_dereference(instructions, offset, repr);
                offset += size_of_repr(repr);
            }
        }
    }
}

#[derive(Debug)]
enum Representation {
    Void,
    Scalar(ValType),
    Vector(Vec<Representation>),
}

fn represented_by(ctx: &IRContext, kind: usize) -> Representation {
    use Representation::*;

    match ctx.kind(kind) {
        IRType::Bool
        | IRType::Unique(_)
        | IRType::Shared(_)
        | IRType::Number(NumericType::Int32) => Scalar(ValType::I32),
        IRType::Number(NumericType::Float32) => Scalar(ValType::F32),
        IRType::Number(NumericType::Int64) => Scalar(ValType::I64),
        IRType::Number(NumericType::Float64) => Scalar(ValType::F64),
        IRType::Struct { fields } => {
            // TODO: don't constantly re-calculate struct reprs
            // TODO: support re-ordering of fields
            Vector(
                fields
                    .values()
                    .map(|field| represented_by(ctx, *field))
                    .collect(),
            )
        }
        IRType::Function { .. } => Void,
        IRType::Void => Void,
        IRType::Unresolved(..) => Void,
    }
}

fn flatten_repr(repr: &Representation, buffer: &mut Vec<ValType>) {
    match repr {
        Representation::Void => {}
        Representation::Scalar(val) => buffer.push(*val),
        Representation::Vector(reprs) => {
            for repr in reprs.iter() {
                flatten_repr(repr, buffer);
            }
        }
    }
}

fn size_of_repr(repr: &Representation) -> u32 {
    match repr {
        Representation::Void => 0,
        Representation::Scalar(val) => value_size(*val),
        Representation::Vector(reprs) => reprs.iter().map(size_of_repr).sum(),
    }
}

fn value_size(val: ValType) -> u32 {
    match val {
        ValType::I32 => 4,
        ValType::F32 => 4,
        ValType::I64 => 8,
        ValType::F64 => 8,
        _ => todo!(),
    }
}

fn emit_lvalue(ctx: &mut EmitContext, lvalue: usize) -> u32 {
    let expr = ctx.arena.expression(lvalue);
    match &expr.value {
        IRExpressionValue::LocalVariable(name) => {
            ctx.add_instruction(Instruction::GlobalGet(BASE_PTR));
            // TODO: don't unwrap?
            *ctx.stack_offsets.get(name.as_str()).unwrap()
        }
        IRExpressionValue::Dereference(child) => {
            let offset = emit_lvalue(ctx, *child) as u64;
            let mem_arg = MemArg {
                offset,
                align: 1,
                memory_index: MAIN_MEMORY,
            };
            ctx.add_instruction(Instruction::I32Load(mem_arg));

            0
        }
        _ => unreachable!(),
    }
}
