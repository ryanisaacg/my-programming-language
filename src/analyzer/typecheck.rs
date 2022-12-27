use std::collections::HashMap;

use crate::{
    analyzer::{BinOpComparison, BinOpNumeric, NumericType},
    parser::{AstNode, AstNodeValue, BinOp},
};

use super::{
    FunDecl, FunctionParameter, IRContext, IRNode, IRNodeValue, IRType, Scope, TypecheckError,
    BOOL_KIND, F32_KIND, F64_KIND, I32_KIND, I64_KIND, VOID_KIND,
};
// TODO: unification of separate IRContexts?

pub fn typecheck(
    statements: impl Iterator<Item = usize>,
    ir_context: &mut IRContext,
    parse_context: &[AstNode],
    scopes: &[Scope],
) -> Result<Vec<IRNode>, TypecheckError> {
    // TODO: cons cell instead?
    let mut local_scope = Vec::with_capacity(1 + scopes.len());
    local_scope.push(Scope {
        declarations: HashMap::new(),
        return_type: None,
    });
    local_scope.extend(scopes.iter().cloned());

    let statements = statements
        .map(|statement| {
            let AstNode { value, start, end } = &parse_context[statement];
            let start = *start;
            let end = *end;

            let value = match value {
                AstNodeValue::Import(_) => {
                    // TODO: disallow inside functions?
                    return Ok(None);
                }
                AstNodeValue::StructDeclaration { .. } => {
                    // TODO: disallow inside functions?
                    return Ok(None);
                }
                AstNodeValue::Return(expr) => {
                    let mut expr = typecheck_expression(
                        &parse_context[*expr],
                        parse_context,
                        ir_context,
                        &local_scope[..],
                    )?;
                    for level in scopes {
                        if let Some(return_type) = level.return_type {
                            expr = deref_until_parity(ir_context, return_type, expr);
                            expr = maybe_promote(ir_context, expr, return_type);
                            if !are_types_equal(ir_context, return_type, expr.kind) {
                                return Err(TypecheckError::UnexpectedType {
                                    found: ir_context.kind(expr.kind).clone(),
                                    expected: ir_context.kind(return_type).clone(),
                                    provenance: expr.start,
                                });
                            }
                            break;
                        }
                    }
                    IRNodeValue::Return(ir_context.add_node(expr))
                }
                AstNodeValue::Expression(expr) => {
                    let expr = typecheck_expression(
                        &parse_context[*expr],
                        parse_context,
                        ir_context,
                        &local_scope[..],
                    )?;
                    IRNodeValue::Expression(ir_context.add_node(expr))
                }
                AstNodeValue::Declaration(name, expr) => {
                    let expr = typecheck_expression(
                        &parse_context[*expr],
                        parse_context,
                        ir_context,
                        &local_scope[..],
                    )?;
                    local_scope[0]
                        .declarations
                        .insert(name.to_string(), expr.kind);
                    IRNodeValue::Declaration(name.clone(), ir_context.add_node(expr))
                }
                AstNodeValue::ExternFunctionBinding {
                    name,
                    params,
                    returns,
                } => {
                    // TODO: do not allow complex types
                    let params = params
                        .iter()
                        .map(|param| {
                            let kind = resolve_ast_type(
                                &parse_context[param.kind],
                                parse_context,
                                ir_context,
                                &local_scope[..],
                            )?;
                            Ok(FunctionParameter {
                                name: param.name.to_string(),
                                kind,
                            })
                        })
                        .collect::<Result<Vec<_>, _>>()?;

                    let returns = returns
                        .as_ref()
                        .map(|type_name| {
                            resolve_ast_type(
                                &parse_context[*type_name],
                                parse_context,
                                ir_context,
                                &local_scope[..],
                            )
                        })
                        .unwrap_or(Ok(VOID_KIND))?;

                    // TODO: do we need to insert the function into the global scope?
                    // I think scan handles that

                    let mut local_scope = local_scope.clone();
                    local_scope.insert(
                        0,
                        Scope {
                            declarations: params
                                .iter()
                                .map(|FunctionParameter { name, kind }| (name.to_string(), *kind))
                                .collect(),
                            return_type: Some(returns),
                        },
                    );

                    IRNodeValue::ExternFunctionBinding {
                        name: name.to_string(),
                        params,
                        returns,
                    }
                }
                AstNodeValue::FunctionDeclaration {
                    name,
                    params,
                    returns,
                    body,
                    is_extern,
                } => {
                    let params = params
                        .iter()
                        .map(|param| {
                            let kind = resolve_ast_type(
                                &parse_context[param.kind],
                                parse_context,
                                ir_context,
                                &local_scope[..],
                            )?;
                            Ok(FunctionParameter {
                                name: param.name.to_string(),
                                kind,
                            })
                        })
                        .collect::<Result<Vec<_>, _>>()?;

                    let returns = returns
                        .as_ref()
                        .map(|type_name| {
                            resolve_ast_type(
                                &parse_context[*type_name],
                                parse_context,
                                ir_context,
                                &local_scope[..],
                            )
                        })
                        .unwrap_or(Ok(VOID_KIND))?;
                    let function_type = ir_context.add_kind(IRType::Function {
                        parameters: params.iter().map(|param| param.kind).collect(),
                        returns,
                    });

                    local_scope[0]
                        .declarations
                        .insert(name.clone(), function_type);

                    let mut local_scope = local_scope.clone();
                    local_scope.insert(
                        0,
                        Scope {
                            declarations: params
                                .iter()
                                .map(|FunctionParameter { name, kind }| (name.to_string(), *kind))
                                .collect(),
                            return_type: Some(returns),
                        },
                    );
                    let body = typecheck_expression(
                        &parse_context[*body],
                        parse_context,
                        ir_context,
                        &local_scope[..],
                    )?;
                    let body = ir_context.add_node(body);
                    check_returns(ir_context, returns, body)?;

                    // TODO: verify body actually returns that type
                    IRNodeValue::FunctionDeclaration(FunDecl {
                        name: name.to_string(),
                        params,
                        returns,
                        body,
                        is_extern: *is_extern,
                    })
                }
                _ => todo!("handle loose statements"),
            };

            Ok(Some(IRNode {
                value,
                start,
                end,
                kind: VOID_KIND,
            }))
        })
        .filter_map(|x| x.transpose())
        .collect::<Result<Vec<IRNode>, TypecheckError>>()?;

    Ok(statements)
}

// TODO: allow accessing the local environment
fn typecheck_expression(
    expression: &AstNode,
    parse_context: &[AstNode],
    ir_context: &mut IRContext,
    local_scope: &[Scope],
) -> Result<IRNode, TypecheckError> {
    use AstNodeValue::*;
    let AstNode { value, start, end } = expression;
    let start = *start;
    let end = *end;

    // TODO: analyze if, block
    Ok(match value {
        ArrayLiteralLength(child, length) => {
            let child = typecheck_expression(
                &parse_context[*child],
                parse_context,
                ir_context,
                local_scope,
            )?;
            let kind = IRType::Array(child.kind);
            let kind = ir_context.add_kind(kind);
            let child = ir_context.add_node(child);

            IRNode {
                value: IRNodeValue::ArrayLiteralLength(child, *length),
                kind,
                start,
                end,
            }
        }
        ArrayLiteral(_) => {
            todo!();
        }
        BinExpr(BinOp::Index, left, right) => {
            // TODO: apply auto-dereferencing
            let left = typecheck_expression(
                &parse_context[*left],
                parse_context,
                ir_context,
                local_scope,
            )?;
            let right = typecheck_expression(
                &parse_context[*right],
                parse_context,
                ir_context,
                local_scope,
            )?;
            let left_kind = ir_context.kind(left.kind);
            let IRType::Array(inner) = left_kind else {
                return Err(TypecheckError::IllegalNonArrayIndex(left_kind.clone(), left.end));
            };
            let right_kind = ir_context.kind(right.kind);
            if !matches!(right_kind, IRType::Number(NumericType::Int32)) {
                return Err(TypecheckError::IllegalNonNumericIndex(
                    right_kind.clone(),
                    right.end,
                ));
            }

            let kind = *inner;
            let left = ir_context.add_node(left);
            let right = ir_context.add_node(right);

            IRNode {
                value: IRNodeValue::ArrayIndex(left, right),
                kind,
                start,
                end,
            }
        }
        StructLiteral { name, fields } => {
            let struct_type_idx = resolve(local_scope, name.as_str());
            let struct_type = struct_type_idx.map(|idx| ir_context.kind(idx));
            if let Some(IRType::Struct {
                fields: type_fields,
            }) = struct_type
            {
                let type_fields = type_fields.clone();
                let mut expr_fields = HashMap::new();
                let mut missing_fields = Vec::new();
                for (field, expected_type_idx) in type_fields.iter() {
                    if let Some(provided_value) = fields.get(field) {
                        let mut provided_value = typecheck_expression(
                            &parse_context[*provided_value],
                            parse_context,
                            ir_context,
                            local_scope,
                        )?;
                        provided_value =
                            deref_until_parity(ir_context, *expected_type_idx, provided_value);
                        provided_value =
                            maybe_promote(ir_context, provided_value, *expected_type_idx);
                        let provided_value = ir_context.add_node(provided_value);
                        expr_fields.insert(field.clone(), provided_value);
                    } else {
                        missing_fields.push(field.clone());
                    }
                }

                let mut extra_fields = Vec::new();
                for field in fields.keys() {
                    if type_fields.get(field).is_none() {
                        extra_fields.push(field.clone());
                    }
                }

                IRNode {
                    value: IRNodeValue::StructLiteral(expr_fields),
                    kind: struct_type_idx.unwrap(),
                    start,
                    end,
                }
            } else {
                return Err(TypecheckError::UnknownName(name.clone(), start));
            }
        }
        BinExpr(BinOp::Dot, left, right) => {
            let mut left = typecheck_expression(
                &parse_context[*left],
                parse_context,
                ir_context,
                local_scope,
            )?;
            while is_pointer(&left, ir_context) {
                left = maybe_dereference(left, ir_context);
            }
            let left_kind = ir_context.kind(left.kind);
            let IRType::Struct { fields } = left_kind else {
                return Err(TypecheckError::IllegalLeftDotOperand(left_kind.clone(), start));
            };
            let AstNode{ value: Name(name), .. } = &parse_context[*right] else {
                return Err(TypecheckError::IllegalRightHandDotOperand(start));
            };
            let Some(field) = fields.get(name) else {
                return Err(TypecheckError::FieldNotFound(name.clone(), left_kind.clone(), start));
            };
            let kind = *field;
            let left = ir_context.add_node(left);
            IRNode {
                value: IRNodeValue::Dot(left, name.clone()),
                kind,
                start,
                end,
            }
        }
        BinExpr(BinOp::Assignment, lvalue, rvalue) => {
            // TODO: provide more error diagnostics
            let mut lvalue = match &parse_context[*lvalue] {
                expr @ AstNode {
                    value: Name(_) | BinExpr(BinOp::Dot, _, _) | BinExpr(BinOp::Index, _, _),
                    ..
                } => typecheck_expression(expr, parse_context, ir_context, local_scope)?,
                AstNode { start, .. } => return Err(TypecheckError::IllegalLeftHandValue(*start)),
            };
            let rvalue = &parse_context[*rvalue];
            let mut rvalue = typecheck_expression(rvalue, parse_context, ir_context, local_scope)?;

            let l_derefs_required = derefs_for_parity(ir_context, rvalue.kind, lvalue.kind);
            for _ in 0..l_derefs_required {
                if matches!(ir_context.kind(lvalue.kind), IRType::Shared(_)) {
                    return Err(TypecheckError::AssignToSharedReference(lvalue.end));
                }
                lvalue = maybe_dereference(lvalue, ir_context);
            }
            rvalue = deref_until_parity(ir_context, lvalue.kind, rvalue);
            rvalue = maybe_promote(ir_context, rvalue, lvalue.kind);

            let l_kind = ir_context.kind(lvalue.kind);
            let r_kind = ir_context.kind(rvalue.kind);
            if !are_types_equal(ir_context, lvalue.kind, rvalue.kind) {
                return Err(TypecheckError::UnexpectedType {
                    found: r_kind.clone(),
                    expected: l_kind.clone(),
                    provenance: start,
                });
            }

            let lvalue = ir_context.add_node(lvalue);
            let rvalue = ir_context.add_node(rvalue);
            IRNode {
                value: IRNodeValue::Assignment(lvalue, rvalue),
                kind: ir_context.add_kind(IRType::Void),
                start,
                end,
            }
        }
        Call(function, arguments) => {
            let expr = &parse_context[*function];
            let function = typecheck_expression(expr, parse_context, ir_context, local_scope)?;
            let (parameter_types, returns) = match ir_context.kind(function.kind).clone() {
                IRType::Function {
                    parameters,
                    returns,
                } => (parameters, returns),
                other => return Err(TypecheckError::NonCallableExpression(other, function.end)),
            };

            if arguments.len() != parameter_types.len() {
                return Err(TypecheckError::WrongArgumentCount {
                    found: arguments.len(),
                    expected: parameter_types.len(),
                    provenance: expr.start,
                });
            }

            let arguments = arguments
                .iter()
                .enumerate()
                .map(|(index, argument)| {
                    let argument = &parse_context[*argument];
                    let mut argument =
                        typecheck_expression(argument, parse_context, ir_context, local_scope)?;
                    argument = deref_until_parity(ir_context, parameter_types[index], argument);
                    argument = maybe_promote(ir_context, argument, parameter_types[index]);
                    if are_types_equal(ir_context, argument.kind, parameter_types[index]) {
                        Ok(ir_context.add_node(argument))
                    } else {
                        let argument_kind = ir_context.kind(argument.kind);
                        let parameter_kind = ir_context.kind(parameter_types[index]);
                        Err(TypecheckError::UnexpectedType {
                            found: argument_kind.clone(),
                            expected: parameter_kind.clone(),
                            provenance: argument.start,
                        })
                    }
                })
                .collect::<Result<Vec<_>, _>>()?;

            let function = ir_context.add_node(function);

            IRNode {
                value: IRNodeValue::Call(function, arguments),
                kind: returns,
                start,
                end,
            }
        }
        Name(name) => {
            let local_type = resolve(local_scope, name);
            match local_type {
                Some(local_type) => IRNode {
                    value: IRNodeValue::LocalVariable(name.clone()),
                    kind: local_type,
                    start,
                    end,
                },
                None => {
                    return Err(TypecheckError::UnknownName(name.clone(), start));
                }
            }
        }
        Bool(val) => IRNode {
            value: IRNodeValue::Bool(*val),
            kind: ir_context.add_kind(IRType::Bool),
            start,
            end,
        },
        // TODO: support bigger-than-32-bit literals
        Int(val) => IRNode {
            value: IRNodeValue::Int(*val),
            kind: I32_KIND,
            start,
            end,
        },
        Float(val) => IRNode {
            value: IRNodeValue::Float(*val),
            kind: F32_KIND,
            start,
            end,
        },
        TakeUnique(child) | TakeShared(child) => {
            let child = &parse_context[*child];
            let child = typecheck_expression(child, parse_context, ir_context, local_scope)?;
            let kind = ir_context.add_kind(match value {
                TakeUnique(_) => IRType::Unique(child.kind),
                TakeShared(_) => IRType::Shared(child.kind),
                _ => unreachable!(),
            });
            let child = ir_context.add_node(child);
            IRNode {
                value: match value {
                    TakeUnique(_) => IRNodeValue::TakeUnique(child),
                    TakeShared(_) => IRNodeValue::TakeShared(child),
                    _ => unreachable!(),
                },
                kind,
                start,
                end,
            }
        }
        If(predicate, block) => {
            let mut predicate = typecheck_expression(
                &parse_context[*predicate],
                parse_context,
                ir_context,
                local_scope,
            )?;
            while is_pointer(&predicate, ir_context) {
                predicate = maybe_dereference(predicate, ir_context);
            }
            if ir_context.kind(predicate.kind) != &IRType::Bool {
                todo!(); // compiler diagnostic
            }
            let block = typecheck_expression(
                &parse_context[*block],
                parse_context,
                ir_context,
                local_scope,
            )?;
            // TODO: if-else can return types
            IRNode {
                value: IRNodeValue::If(ir_context.add_node(predicate), ir_context.add_node(block)),
                kind: ir_context.add_kind(IRType::Void),
                start,
                end,
            }
        }
        // TODO
        While(predicate, block) => {
            let predicate = typecheck_expression(
                &parse_context[*predicate],
                parse_context,
                ir_context,
                local_scope,
            )?;
            let predicate_type = ir_context.kind(predicate.kind);
            if predicate_type != &IRType::Bool {
                todo!(); // compiler diagnostic
            }
            let block = typecheck_expression(
                &parse_context[*block],
                parse_context,
                ir_context,
                local_scope,
            )?;
            // TODO: if-else can return types
            IRNode {
                value: IRNodeValue::While(
                    ir_context.add_node(predicate),
                    ir_context.add_node(block),
                ),
                kind: ir_context.add_kind(IRType::Void),
                start,
                end,
            }
        }
        Block(parse_statements) => {
            let ir_statements = typecheck(
                parse_statements.iter().cloned(),
                ir_context,
                parse_context,
                local_scope,
            )?;
            let ir_statements = ir_statements
                .into_iter()
                .map(|statement| ir_context.add_node(statement))
                .collect();
            // TODO: support returning last element if non-semicolon
            IRNode {
                value: IRNodeValue::Block(ir_statements),
                kind: ir_context.add_kind(IRType::Void),
                start,
                end,
            }
        }
        BinExpr(
            op @ (BinOp::Add
            | BinOp::Subtract
            | BinOp::Multiply
            | BinOp::Divide
            | BinOp::LessThan
            | BinOp::GreaterThan),
            left,
            right,
        ) => {
            let mut left = typecheck_expression(
                &parse_context[*left],
                parse_context,
                ir_context,
                local_scope,
            )?;
            let mut right = typecheck_expression(
                &parse_context[*right],
                parse_context,
                ir_context,
                local_scope,
            )?;
            while is_pointer(&left, ir_context) {
                left = maybe_dereference(left, ir_context);
            }
            while is_pointer(&right, ir_context) {
                right = maybe_dereference(right, ir_context);
            }
            let left_kind = ir_context.kind(left.kind);
            let IRType::Number(left_num) = left_kind else {
                return Err(TypecheckError::BinaryNonNumericOperand(left_kind.clone(), left.start));
            };
            let right_kind = ir_context.kind(right.kind);
            let IRType::Number(right_num) = right_kind else {
                return Err(TypecheckError::BinaryNonNumericOperand(right_kind.clone(), right.start));
            };
            match which_wider(*left_num, *right_num) {
                Wider::Left => {
                    right = maybe_promote(ir_context, right, left.kind);
                }
                Wider::Right => {
                    left = maybe_promote(ir_context, left, right.kind);
                }
                Wider::Neither => {
                    return Err(TypecheckError::BinaryOperandMismatch(
                        left_kind.clone(),
                        right_kind.clone(),
                        start,
                    ));
                }
                Wider::Equal => {}
            }
            let left_type = left.kind;
            if *op == BinOp::Add
                || *op == BinOp::Subtract
                || *op == BinOp::Multiply
                || *op == BinOp::Divide
            {
                IRNode {
                    value: IRNodeValue::BinaryNumeric(
                        match op {
                            BinOp::Add => BinOpNumeric::Add,
                            BinOp::Subtract => BinOpNumeric::Subtract,
                            BinOp::Multiply => BinOpNumeric::Multiply,
                            BinOp::Divide => BinOpNumeric::Divide,
                            _ => unreachable!(),
                        },
                        ir_context.add_node(left),
                        ir_context.add_node(right),
                    ),
                    kind: left_type,
                    start,
                    end,
                }
            } else {
                IRNode {
                    value: IRNodeValue::Comparison(
                        match op {
                            BinOp::LessThan => BinOpComparison::LessThan,
                            BinOp::GreaterThan => BinOpComparison::GreaterThan,
                            _ => unreachable!(),
                        },
                        ir_context.add_node(left),
                        ir_context.add_node(right),
                    ),
                    kind: ir_context.add_kind(IRType::Bool),
                    start,
                    end,
                }
            }
        }
        other => todo!("nested top-level declarations, {:?}", other),
    })
}

fn deref_until_parity(
    ir_context: &mut IRContext,
    benchmark: usize,
    mut expression: IRNode,
) -> IRNode {
    let derefs = derefs_for_parity(ir_context, benchmark, expression.kind);
    for _ in 0..derefs {
        expression = maybe_dereference(expression, ir_context);
    }

    expression
}

fn derefs_for_parity(ir_context: &IRContext, benchmark_idx: usize, argument: usize) -> u32 {
    let benchmark = ir_context.kind(benchmark_idx);
    let argument = ir_context.kind(argument);
    match (benchmark, argument) {
        (
            IRType::Unique(benchmark) | IRType::Shared(benchmark),
            IRType::Unique(argument) | IRType::Shared(argument),
        ) => derefs_for_parity(ir_context, *benchmark, *argument),
        (IRType::Unique(_benchmark) | IRType::Shared(_benchmark), _) => {
            0 // TODO: should this indicate an error state?
        }
        (_, IRType::Unique(argument) | IRType::Shared(argument)) => {
            derefs_for_parity(ir_context, benchmark_idx, *argument) + 1
        }
        (_benchmark, _guardian) => 0,
    }
}

/**
 * Dereference the expression if it is a pointer type
 */
fn maybe_dereference(expression: IRNode, ir_context: &mut IRContext) -> IRNode {
    let kind = ir_context.kind(expression.kind);
    if let IRType::Unique(inner) | IRType::Shared(inner) = kind {
        let kind = *inner;
        let start = expression.start;
        let end = expression.end;
        let child = ir_context.add_node(expression);
        IRNode {
            value: IRNodeValue::Dereference(child),
            kind,
            start,
            end,
        }
    } else {
        expression
    }
}

/**
 * Promote the numeric type if it would be required
 */
fn maybe_promote(ir_context: &mut IRContext, expression: IRNode, target_kind: usize) -> IRNode {
    let kind = ir_context.kind(expression.kind);
    let IRType::Number(target) = ir_context.kind(target_kind) else {
        return expression;
    };
    if let IRType::Number(number) = kind {
        if which_wider(*number, *target) == Wider::Right {
            // TODO: only promote
            let start = expression.start;
            let end = expression.end;
            let child = ir_context.add_node(expression);
            return IRNode {
                value: IRNodeValue::Promote(child),
                kind: target_kind,
                start,
                end,
            };
        }
    }

    expression
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum Wider {
    Left,
    Right,
    Equal,
    Neither,
}

fn which_wider(a: NumericType, b: NumericType) -> Wider {
    use NumericType::*;
    match (a, b) {
        (Int32, Int64) => Wider::Right,
        (Float32, Float64) => Wider::Right,

        (Int32, Float32) => Wider::Right,
        (Int32, Float64) => Wider::Right,
        (Int64, Float64) => Wider::Right,

        (Int64, Int32) => Wider::Left,
        (Float64, Float32) => Wider::Left,

        (Float32, Int32) => Wider::Left,
        (Float64, Int32) => Wider::Left,
        (Float64, Int64) => Wider::Left,

        (Float32, Int64) => Wider::Neither,
        (Int64, Float32) => Wider::Neither,

        (Int32, Int32) => Wider::Equal,
        (Int64, Int64) => Wider::Equal,
        (Float32, Float32) => Wider::Equal,
        (Float64, Float64) => Wider::Equal,
    }
}

fn is_pointer(expression: &IRNode, ir_context: &IRContext) -> bool {
    let kind = ir_context.kind(expression.kind);

    matches!(kind, IRType::Unique(_) | IRType::Shared(_))
}

fn resolve_ast_type(
    ast_type: &AstNode,
    parse_context: &[AstNode],
    ir_context: &mut IRContext,
    scope: &[Scope],
) -> Result<usize, TypecheckError> {
    Ok(match &ast_type.value {
        AstNodeValue::Name(string) => match string.as_str() {
            "void" => VOID_KIND,
            "bool" => BOOL_KIND,
            "i64" => I64_KIND,
            "f64" => F64_KIND,
            "i32" => I32_KIND,
            "f32" => F32_KIND,
            name => resolve(scope, name)
                .ok_or_else(|| TypecheckError::UnknownName(name.to_string(), ast_type.start))?,
        },
        pointer @ (AstNodeValue::UniqueType(inner)
        | AstNodeValue::SharedType(inner)
        | AstNodeValue::ArrayType(inner)) => {
            let inner = &parse_context[*inner];
            let inner = resolve_ast_type(inner, parse_context, ir_context, scope)?;

            match pointer {
                AstNodeValue::UniqueType(_) => ir_context.add_kind(IRType::Unique(inner)),
                AstNodeValue::SharedType(_) => ir_context.add_kind(IRType::Shared(inner)),
                AstNodeValue::ArrayType(_) => ir_context.add_kind(IRType::Array(inner)),
                _ => unreachable!(),
            }
        }
        _ => todo!("ast type"),
    })
}

fn resolve(scope: &[Scope], name: &str) -> Option<usize> {
    for level in scope {
        if let Some(kind) = level.declarations.get(name) {
            return Some(*kind);
        }
    }

    None
}

fn are_types_equal(ir_context: &IRContext, a: usize, b: usize) -> bool {
    use IRType::*;
    let a = ir_context.kind(a);
    let b = ir_context.kind(b);
    match (a, b) {
        (Unique(a), Unique(b)) | (Shared(a), Shared(b)) => are_types_equal(ir_context, *a, *b),
        (
            Function {
                parameters: a_param,
                returns: a_returns,
            },
            Function {
                parameters: b_param,
                returns: b_returns,
            },
        ) => {
            a_param.len() == b_param.len()
                && a_param
                    .iter()
                    .zip(b_param.iter())
                    .all(|(a, b)| are_types_equal(ir_context, *a, *b))
                && are_types_equal(ir_context, *a_returns, *b_returns)
        }
        (Struct { fields: a_fields }, Struct { fields: b_fields }) => {
            a_fields.iter().all(|(a_key, a_value)| {
                if let Some(b_value) = b_fields.get(a_key) {
                    are_types_equal(ir_context, *a_value, *b_value)
                } else {
                    false
                }
            }) && b_fields.iter().all(|(b_key, b_value)| {
                if let Some(a_value) = a_fields.get(b_key) {
                    are_types_equal(ir_context, *a_value, *b_value)
                } else {
                    false
                }
            })
        }
        (Void | Bool | Number(_), Void | Bool | Number(_)) => a == b,
        _ => false,
    }
}

fn check_returns(
    ir_context: &mut IRContext,
    return_type: usize,
    body: usize,
) -> Result<(), TypecheckError> {
    let body_expr = ir_context.node(body);
    if !are_types_equal(ir_context, return_type, body_expr.kind) {
        // TODO: check if a 'return' statement is guaranteed to be reached
        // TODO: attempt to repair the body, and return an error if it is impsosible
    }

    Ok(())
}
