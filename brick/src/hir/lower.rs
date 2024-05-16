use std::collections::HashMap;

use super::{
    ArithmeticOp, BinaryLogicalOp, ComparisonOp, GeneratorProperties, HirFunction, HirModule,
    HirNode, HirNodeValue, UnaryLogicalOp,
};

use crate::{
    id::{FunctionID, NodeID, TypeID, VariableID},
    parser::{AstNode, AstNodeValue, BinOp, IfDeclaration, UnaryOp},
    typecheck::{
        find_func, fully_dereference, traverse_dots, CollectionType, ExpressionType, FuncType,
        PointerKind, PrimitiveType, StaticDeclaration, TypecheckedFile, TypecheckedFunction,
    },
};

pub fn lower_module(
    module: TypecheckedFile<'_>,
    declarations: &HashMap<TypeID, &StaticDeclaration>,
) -> HirModule {
    let TypecheckedFile {
        functions,
        top_level_statements,
    } = module;

    let mut module_functions = Vec::with_capacity(functions.len());
    for func in functions {
        let func_ty = find_func(declarations, func.id).unwrap();
        if func_ty.is_coroutine {
            module_functions.extend(lower_coroutine(declarations, func, func_ty));
        } else {
            module_functions.push(lower_function(declarations, func, func_ty));
        }
    }

    let top_level_ty = top_level_statements
        .last()
        .and_then(|last| last.ty.get().cloned());
    HirModule {
        functions: module_functions,
        top_level_statements: HirNode::autogenerated(
            HirNodeValue::Sequence(
                top_level_statements
                    .into_iter()
                    .map(|stmt| lower_node(declarations, stmt))
                    .collect(),
            ),
            top_level_ty.unwrap_or(ExpressionType::Void),
        ),
    }
}

fn lower_coroutine(
    decls: &HashMap<TypeID, &StaticDeclaration>,
    func: TypecheckedFunction<'_>,
    func_ty: &FuncType,
) -> [HirFunction; 2] {
    let generator_function_id = FunctionID::new();

    let coroutine_start = HirFunction {
        id: func.id,
        name: Some(func.name),
        body: HirNode::autogenerated(
            HirNodeValue::Sequence(vec![HirNode::autogenerated(
                HirNodeValue::GeneratorCreate {
                    generator_function: generator_function_id,
                    // TODO: pass the parameters down
                    args: vec![],
                },
                func_ty.returns.clone(),
            )]),
            func_ty.returns.clone(),
        ),
        // TODO: pass param types down
        params: Vec::new(),
        generator: None,
    };

    // TODO: pass generator into generator function body
    let mut body = lower_node(decls, func.func.body);
    let HirNodeValue::Sequence(instrs) = &mut body.value else {
        unreachable!()
    };
    let generator_var_id = VariableID::new();
    let generator_ty =
        ExpressionType::Pointer(PointerKind::Unique, Box::new(func_ty.returns.clone()));
    let mut body_instrs = vec![HirNode::autogenerated(
        HirNodeValue::Parameter(0, generator_var_id),
        generator_ty.clone(),
    )];
    let ExpressionType::Generator { param_ty, .. } = &func_ty.returns else {
        unreachable!()
    };
    let mut param_types = vec![generator_ty.clone()];
    let param_var_id = if param_ty.as_ref() != &ExpressionType::Void {
        let var_id = VariableID::new();
        body_instrs.push(HirNode::autogenerated(
            HirNodeValue::Parameter(1, var_id),
            *param_ty.clone(),
        ));
        param_types.push(*param_ty.clone());
        Some(var_id)
    } else {
        None
    };
    body_instrs.push(HirNode::autogenerated(
        HirNodeValue::GeneratorResume(Box::new(HirNode::autogenerated(
            HirNodeValue::Access(
                Box::new(HirNode::autogenerated(
                    HirNodeValue::VariableReference(generator_var_id.into()),
                    generator_ty.clone(),
                )),
                "resume_point".to_string(),
            ),
            ExpressionType::Primitive(PrimitiveType::PointerSize),
        ))),
        ExpressionType::Void,
    ));
    body_instrs.push(HirNode::autogenerated(
        HirNodeValue::GotoLabel(0),
        ExpressionType::Void,
    ));
    body_instrs.append(instrs);
    body.value = HirNodeValue::Sequence(body_instrs);

    let coroutine_body = HirFunction {
        id: generator_function_id,
        name: None,
        body,
        generator: Some(GeneratorProperties {
            generator_var_id,
            param_var_id,
            ty: generator_ty.clone(),
        }),
        params: param_types,
    };

    [coroutine_start, coroutine_body]
}

fn lower_function(
    decls: &HashMap<TypeID, &StaticDeclaration>,
    func: TypecheckedFunction<'_>,
    func_ty: &FuncType,
) -> HirFunction {
    let mut instructions: Vec<_> = func
        .func
        .params
        .iter()
        .enumerate()
        .map(|(i, (id, _param))| {
            HirNode::autogenerated(HirNodeValue::Parameter(i, *id), func_ty.params[i].clone())
        })
        .collect();
    let mut body = lower_node(decls, func.func.body);
    let HirNodeValue::Sequence(instrs) = &mut body.value else {
        unreachable!()
    };
    instructions.append(instrs);
    std::mem::swap(&mut instructions, instrs);
    HirFunction {
        id: func.id,
        name: Some(func.name),
        body,
        generator: None,
        params: func_ty.params.clone(),
    }
}

fn lower_node(decls: &HashMap<TypeID, &StaticDeclaration>, node: &AstNode<'_>) -> HirNode {
    let value = match &node.value {
        AstNodeValue::Int(x) => HirNodeValue::Int(*x),
        AstNodeValue::Float(x) => HirNodeValue::Float(*x),
        AstNodeValue::Bool(x) => HirNodeValue::Bool(*x),
        AstNodeValue::Null => HirNodeValue::Null,
        AstNodeValue::CharLiteral(x) => HirNodeValue::CharLiteral(*x),
        AstNodeValue::StringLiteral(x) => HirNodeValue::StringLiteral(x.clone()),

        AstNodeValue::While(cond, body) => {
            // TODO: can you assign out of a while?
            let cond = lower_node_alloc(decls, cond);
            let body = lower_node_alloc(decls, body);
            HirNodeValue::While(cond, body)
        }
        AstNodeValue::Loop(body) => HirNodeValue::Loop(lower_node_alloc(decls, body)),
        AstNodeValue::Block(contents) => {
            let contents = contents
                .iter()
                .map(|node| lower_node(decls, node))
                .collect();

            HirNodeValue::Sequence(contents)
        }
        // TODO
        AstNodeValue::If(IfDeclaration {
            condition,
            if_branch,
            else_branch,
        }) => {
            let condition = lower_node_alloc(decls, condition);
            let if_branch = lower_node_alloc(decls, if_branch);
            let else_branch = else_branch
                .as_ref()
                .map(|else_branch| lower_node_alloc(decls, else_branch));

            HirNodeValue::If(condition, if_branch, else_branch)
        }

        AstNodeValue::TakeUnique(inner) => HirNodeValue::TakeUnique(lower_node_alloc(decls, inner)),
        AstNodeValue::TakeRef(inner) => HirNodeValue::TakeShared(lower_node_alloc(decls, inner)),
        AstNodeValue::Deref(inner) => HirNodeValue::Dereference(lower_node_alloc(decls, inner)),

        // Statement doesn't actually add a node - the inner expression
        // has what really counts
        AstNodeValue::Statement(inner) => return lower_node(decls, inner),

        AstNodeValue::Declaration(_lvalue, type_hint, rvalue, variable_id) => {
            let ty = type_hint
                .as_ref()
                .map(|node| node.ty.get().unwrap().clone())
                .unwrap_or_else(|| rvalue.ty.get().unwrap().clone());
            let lvalue = Box::new(HirNode {
                id: NodeID::new(),
                value: HirNodeValue::VariableReference((*variable_id).into()),
                ty: ty.clone(),
                provenance: Some(node.provenance.clone()),
            });
            let rvalue = lower_node_alloc(decls, rvalue);
            let statements = vec![
                HirNode::from_ast(node, HirNodeValue::Declaration(*variable_id), ty),
                HirNode {
                    id: NodeID::new(),
                    value: HirNodeValue::Assignment(lvalue, rvalue),
                    ty: ExpressionType::Void,
                    provenance: Some(node.provenance.clone()),
                },
            ];
            HirNodeValue::Sequence(statements)
        }
        AstNodeValue::Name { referenced_id, .. } => HirNodeValue::VariableReference(
            *referenced_id.get().expect("referenced ID to be filled in"),
        ),

        AstNodeValue::Return(inner) => {
            HirNodeValue::Return(inner.as_ref().map(|inner| lower_node_alloc(decls, inner)))
        }
        AstNodeValue::Yield(inner) => {
            HirNodeValue::Yield(inner.as_ref().map(|inner| lower_node_alloc(decls, inner)))
        }
        AstNodeValue::BinExpr(BinOp::Dot, left, right) => {
            let expr_ty = fully_dereference(left.ty.get().unwrap());
            let AstNodeValue::Name { value: name, .. } = &right.value else {
                unreachable!()
            };
            if let Some(StaticDeclaration::Module(module)) =
                expr_ty.id().and_then(|id| decls.get(id))
            {
                HirNodeValue::VariableReference(
                    module
                        .exports
                        .get(name)
                        .expect("module export to exist")
                        .id()
                        .into(),
                )
            } else {
                let left = lower_node_alloc(decls, left);
                HirNodeValue::Access(left, name.clone())
            }
        }
        AstNodeValue::BinExpr(BinOp::NullChaining, left, right) => {
            let expr_ty = fully_dereference(left.ty.get().unwrap());
            let AstNodeValue::Name { value: name, .. } = &right.value else {
                unreachable!()
            };
            if let Some(StaticDeclaration::Module(module)) =
                expr_ty.id().and_then(|id| decls.get(id))
            {
                HirNodeValue::VariableReference(
                    module
                        .exports
                        .get(name)
                        .expect("module export to exist")
                        .id()
                        .into(),
                )
            } else {
                let left = lower_node_alloc(decls, left);
                let mut name_list = Vec::new();
                traverse_dots(right, |name| {
                    name_list.push(name.to_string());
                });
                HirNodeValue::NullableTraverse(left, name_list)
            }
        }
        AstNodeValue::BinExpr(BinOp::Index, left, right) => {
            let ty = left.ty.get().unwrap();
            let left = lower_node_alloc(decls, left);
            let right = lower_node_alloc(decls, right);
            match ty {
                ExpressionType::Collection(collection) => match collection {
                    CollectionType::Dict(_, _) => HirNodeValue::DictIndex(left, right),
                    CollectionType::Array(_) => HirNodeValue::ArrayIndex(left, right),
                    CollectionType::String => todo!(),
                },
                _ => unreachable!(),
            }
        }
        AstNodeValue::UnaryExpr(op, child) => {
            let child = lower_node_alloc(decls, child);
            HirNodeValue::UnaryLogical(
                match op {
                    UnaryOp::BooleanNot => UnaryLogicalOp::BooleanNot,
                },
                child,
            )
        }
        AstNodeValue::BinExpr(op, left, right) => {
            let left = lower_node_alloc(decls, left);
            let right = lower_node_alloc(decls, right);

            match op {
                BinOp::AddAssign => HirNodeValue::Assignment(
                    left.clone(),
                    Box::new(HirNode::from_ast_void(
                        node,
                        HirNodeValue::Arithmetic(ArithmeticOp::Add, left, right),
                    )),
                ),
                BinOp::SubtractAssign => HirNodeValue::Assignment(
                    left.clone(),
                    Box::new(HirNode::from_ast_void(
                        node,
                        HirNodeValue::Arithmetic(ArithmeticOp::Subtract, left, right),
                    )),
                ),
                BinOp::MultiplyAssign => HirNodeValue::Assignment(
                    left.clone(),
                    Box::new(HirNode::from_ast_void(
                        node,
                        HirNodeValue::Arithmetic(ArithmeticOp::Multiply, left, right),
                    )),
                ),
                BinOp::DivideAssign => HirNodeValue::Assignment(
                    left.clone(),
                    Box::new(HirNode::from_ast_void(
                        node,
                        HirNodeValue::Arithmetic(ArithmeticOp::Divide, left, right),
                    )),
                ),
                BinOp::Assignment => HirNodeValue::Assignment(left, right),

                BinOp::Add => HirNodeValue::Arithmetic(ArithmeticOp::Add, left, right),
                BinOp::Subtract => HirNodeValue::Arithmetic(ArithmeticOp::Subtract, left, right),
                BinOp::Multiply => HirNodeValue::Arithmetic(ArithmeticOp::Multiply, left, right),
                BinOp::Divide => HirNodeValue::Arithmetic(ArithmeticOp::Divide, left, right),
                BinOp::LessThan => HirNodeValue::Comparison(ComparisonOp::LessThan, left, right),
                BinOp::GreaterThan => {
                    HirNodeValue::Comparison(ComparisonOp::GreaterThan, left, right)
                }
                BinOp::LessEqualThan => {
                    HirNodeValue::Comparison(ComparisonOp::LessEqualThan, left, right)
                }
                BinOp::GreaterEqualThan => {
                    HirNodeValue::Comparison(ComparisonOp::GreaterEqualThan, left, right)
                }
                BinOp::EqualTo => HirNodeValue::Comparison(ComparisonOp::EqualTo, left, right),
                BinOp::NotEquals => HirNodeValue::Comparison(ComparisonOp::NotEquals, left, right),
                BinOp::BooleanAnd => {
                    HirNodeValue::BinaryLogical(BinaryLogicalOp::BooleanAnd, left, right)
                }
                BinOp::BooleanOr => {
                    HirNodeValue::BinaryLogical(BinaryLogicalOp::BooleanOr, left, right)
                }
                BinOp::NullCoalesce => HirNodeValue::NullCoalesce(left, right),
                BinOp::Index | BinOp::Dot | BinOp::NullChaining => unreachable!(),
                BinOp::Concat => HirNodeValue::StringConcat(left, right),
            }
        }
        AstNodeValue::Call(func, params) => {
            let func = lower_node_alloc(decls, func);
            let params = params
                .iter()
                .map(|param| lower_node(decls, param))
                .collect();
            HirNodeValue::Call(func, params)
        }
        AstNodeValue::RecordLiteral { name, fields } => {
            let AstNodeValue::Name { referenced_id, .. } = &name.value else {
                panic!("Struct literal must have Name");
            };
            let id = referenced_id
                .get()
                .expect("referenced fields to be filled in")
                .as_type();
            match decls[&id] {
                StaticDeclaration::Struct(_) => {
                    let fields = fields
                        .iter()
                        .map(|(name, field)| (name.clone(), lower_node(decls, field)))
                        .collect();
                    HirNodeValue::StructLiteral(id, fields)
                }
                StaticDeclaration::Union(_) => {
                    let (variant, node) = fields.iter().next().unwrap();
                    let node = lower_node_alloc(decls, node);
                    HirNodeValue::UnionLiteral(id, variant.clone(), node)
                }
                _ => unreachable!(),
            }
        }
        AstNodeValue::DictLiteral(elements) => HirNodeValue::DictLiteral(
            elements
                .iter()
                .map(|(key, value)| (lower_node(decls, key), lower_node(decls, value)))
                .collect(),
        ),
        AstNodeValue::ArrayLiteral(arr) => {
            let arr = arr.iter().map(|elem| lower_node(decls, elem)).collect();
            HirNodeValue::ArrayLiteral(arr)
        }
        AstNodeValue::ArrayLiteralLength(elem, count) => {
            let elem = lower_node_alloc(decls, elem);
            let count = lower_node_alloc(decls, count);
            HirNodeValue::ArrayLiteralLength(elem, count)
        }

        AstNodeValue::FunctionDeclaration(_)
        | AstNodeValue::ExternFunctionBinding(_)
        | AstNodeValue::StructDeclaration(_)
        | AstNodeValue::UnionDeclaration(_)
        | AstNodeValue::InterfaceDeclaration(_)
        | AstNodeValue::Import(_)
        | AstNodeValue::UniqueType(_)
        | AstNodeValue::VoidType
        | AstNodeValue::SharedType(_)
        | AstNodeValue::NullableType(_)
        | AstNodeValue::RequiredFunction(_)
        | AstNodeValue::DictType(_, _)
        | AstNodeValue::ArrayType(_)
        | AstNodeValue::GeneratorType { .. } => unreachable!("Can't have these in a function body"),
        AstNodeValue::Match(_) => todo!(),
    };

    HirNode::from_ast(node, value, node.ty.get().expect("type filled").clone())
}

fn lower_node_alloc(
    decls: &HashMap<TypeID, &StaticDeclaration>,
    node: &AstNode<'_>,
) -> Box<HirNode> {
    Box::new(lower_node(decls, node))
}
